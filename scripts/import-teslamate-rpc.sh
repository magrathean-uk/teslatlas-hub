#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
set +x
umask 077

readonly PROGRAM_NAME="${0##*/}"
readonly DEFAULT_HUB_COMMAND="/usr/bin/teslatlas-hub"

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage:
  import-teslamate-rpc.sh --source-host HOST --teslamate-container NAME \
    (--hub-host HOST | --local | --mac-keychain) [--source-port PORT] [--hub-port PORT] \
    [--source-sudo] [--hub-sudo] [--hub-command PATH] \
    [--mac-keychain-helper PATH] [--mac-keychain-service NAME] [--mac-keychain-account NAME]

Reads TeslaMate.Auth.get_tokens() through SSH and Docker RPC, validates one
strict access/refresh JSON pair, then sends it directly to the Hub's bounded
host-encrypted stdin helper. No PTY, token argument, token environment value,
plaintext temporary file, or token-bearing output is used.
EOF
}

source_host=''
teslamate_container=''
hub_host=''
local_receiver=false
mac_keychain_receiver=false
hub_command="$DEFAULT_HUB_COMMAND"
source_port=22
hub_port=22
source_sudo=false
hub_sudo=false
mac_keychain_helper="${TESLATLAS_HUB_MAC_KEYCHAIN_HELPER:-$HOME/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub-keychain}"
mac_keychain_service="${TESLATLAS_HUB_MAC_OWNER_SERVICE:-com.teslatlas.hub.owner-tokens.v2}"
mac_keychain_account="${TESLATLAS_HUB_MAC_ACCOUNT:-$(id -un)}"

while (($#)); do
  case "$1" in
    --source-host)
      (($# >= 2)) || die '--source-host requires a value'
      source_host="$2"
      shift 2
      ;;
    --teslamate-container)
      (($# >= 2)) || die '--teslamate-container requires a value'
      teslamate_container="$2"
      shift 2
      ;;
    --hub-host)
      (($# >= 2)) || die '--hub-host requires a value'
      hub_host="$2"
      shift 2
      ;;
    --local)
      local_receiver=true
      shift
      ;;
    --mac-keychain)
      mac_keychain_receiver=true
      shift
      ;;
    --hub-command)
      (($# >= 2)) || die '--hub-command requires a value'
      hub_command="$2"
      shift 2
      ;;
    --source-port)
      (($# >= 2)) || die '--source-port requires a value'
      source_port="$2"
      shift 2
      ;;
    --hub-port)
      (($# >= 2)) || die '--hub-port requires a value'
      hub_port="$2"
      shift 2
      ;;
    --source-sudo)
      source_sudo=true
      shift
      ;;
    --hub-sudo)
      hub_sudo=true
      shift
      ;;
    --mac-keychain-helper)
      (($# >= 2)) || die '--mac-keychain-helper requires a value'
      mac_keychain_helper="$2"
      shift 2
      ;;
    --mac-keychain-service)
      (($# >= 2)) || die '--mac-keychain-service requires a value'
      mac_keychain_service="$2"
      shift 2
      ;;
    --mac-keychain-account)
      (($# >= 2)) || die '--mac-keychain-account requires a value'
      mac_keychain_account="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ -n "$source_host" ]] || die '--source-host is required'
[[ -n "$teslamate_container" ]] || die '--teslamate-container is required'
[[ "$source_host" != *[!A-Za-z0-9.@:_\[\]-]* ]] || die 'source host contains unsafe characters'
[[ "$teslamate_container" =~ ^[A-Za-z0-9_.-]+$ ]] || \
  die 'TeslaMate container name contains unsafe characters'
[[ "$hub_host" != *[!A-Za-z0-9.@:_\[\]-]* ]] || \
  [[ -z "$hub_host" ]] || die 'Hub host contains unsafe characters'
if [[ "$local_receiver" == true && -n "$hub_host" ]]; then
  die '--local and --hub-host cannot be combined'
fi
if [[ "$local_receiver" == true && "$hub_sudo" == true ]]; then
  die '--hub-sudo requires --hub-host'
fi
if [[ "$mac_keychain_receiver" == true && ( "$local_receiver" == true || -n "$hub_host" ) ]]; then
  die '--mac-keychain cannot be combined with --local or --hub-host'
fi
if [[ "$mac_keychain_receiver" == true && "$hub_sudo" == true ]]; then
  die '--hub-sudo requires --hub-host'
fi
if [[ "$local_receiver" == false && "$mac_keychain_receiver" == false && -z "$hub_host" ]]; then
  die 'choose --hub-host, --local, or --mac-keychain'
fi
[[ "$hub_command" = /* && "$hub_command" != *[!A-Za-z0-9_./-]* ]] || \
  die 'Hub command path is unsafe'
if [[ "$mac_keychain_receiver" == true ]]; then
  [[ "$(uname -s)" == Darwin ]] || die '--mac-keychain requires macOS'
  [[ "$mac_keychain_helper" = /* && -x "$mac_keychain_helper" ]] || \
    die 'macOS Keychain helper must be an executable absolute path'
  [[ "$mac_keychain_service" =~ ^[A-Za-z0-9._-]+$ ]] || \
    die 'macOS Keychain service contains unsafe characters'
  [[ "$mac_keychain_account" =~ ^[A-Za-z0-9._-]+$ ]] || \
    die 'macOS Keychain account contains unsafe characters'
fi
validate_port() {
  local name="$1"
  local value="$2"
  [[ "$value" =~ ^[0-9]{1,5}$ ]] || die "$name must be a numeric TCP port"
  (( 10#$value >= 1 && 10#$value <= 65535 )) || die "$name is outside 1-65535"
}
validate_port '--source-port' "$source_port"
validate_port '--hub-port' "$hub_port"
command -v ssh >/dev/null 2>&1 || die 'ssh is required'
command -v python3 >/dev/null 2>&1 || die 'python3 is required'
if [[ "$local_receiver" == true ]]; then
  [[ -x "$hub_command" ]] || die 'Hub command is not executable'
fi

quote_remote_arg() {
  local value="$1"
  printf "'%s'" "${value//\'/\'\\\'\'}"
}

rpc_expression='case TeslaMate.Auth.get_tokens() do %TeslaMate.Auth.Tokens{access: access_token, refresh: refresh_token} -> IO.write(Jason.encode!(%{access_token: access_token, refresh_token: refresh_token})); _ -> raise "TeslaMate owner tokens unavailable" end'
source_remote_command=''
if [[ "$source_sudo" == true ]]; then
  source_remote_command="$(printf '%s %s ' "$(quote_remote_arg sudo)" "$(quote_remote_arg -n)")"
fi
source_remote_command+="$(printf '%s %s %s %s %s %s' \
  "$(quote_remote_arg docker)" \
  "$(quote_remote_arg exec)" \
  "$(quote_remote_arg "$teslamate_container")" \
  "$(quote_remote_arg bin/teslamate)" \
  "$(quote_remote_arg rpc)" \
  "$(quote_remote_arg "$rpc_expression")")"
readonly rpc_expression source_remote_command

run_source_rpc() {
  ssh -T -p "$source_port" -o BatchMode=yes -o LogLevel=ERROR -- "$source_host" \
    "$source_remote_command" 2>/dev/null
}

run_remote_receiver() {
  local remote_receiver
  if [[ "$hub_sudo" == true ]]; then
    remote_receiver="$(printf '%s %s %s %s' \
      "$(quote_remote_arg sudo)" \
      "$(quote_remote_arg -n)" \
      "$(quote_remote_arg "$hub_command")" \
      "$(quote_remote_arg store-tesla-mate-owner-tokens)")"
  else
    remote_receiver="$(printf '%s %s' \
      "$(quote_remote_arg "$hub_command")" \
      "$(quote_remote_arg store-tesla-mate-owner-tokens)")"
  fi
  ssh -T -p "$hub_port" -o BatchMode=yes -o LogLevel=ERROR -- "$hub_host" \
    "$remote_receiver" >/dev/null 2>/dev/null
}

run_local_receiver() {
  "$hub_command" store-tesla-mate-owner-tokens >/dev/null 2>/dev/null
}

run_mac_keychain_receiver() {
  "$mac_keychain_helper" set "$mac_keychain_service" "$mac_keychain_account" \
    >/dev/null 2>/dev/null
}

validate_rpc_json() {
  python3 -c '
import json
import sys

MAX_BYTES = 32 * 1024
MAX_TOKEN_BYTES = 16 * 1024

class DuplicateField(Exception):
    pass

def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise DuplicateField()
        result[key] = value
    return result

def fail():
    raise SystemExit(1)

raw = sys.stdin.buffer.read(MAX_BYTES + 1)
if len(raw) > MAX_BYTES:
    fail()
try:
    text = raw.decode("utf-8")
    decoder = json.JSONDecoder(object_pairs_hook=pairs)
    start = len(text) - len(text.lstrip())
    value, end = decoder.raw_decode(text, start)
    if text[end:].strip():
        fail()
except (UnicodeDecodeError, json.JSONDecodeError, DuplicateField):
    fail()

if not isinstance(value, dict) or set(value) != {"access_token", "refresh_token"}:
    fail()
access = value["access_token"]
refresh = value["refresh_token"]
if not isinstance(access, str) or not isinstance(refresh, str):
    fail()
if not access or not refresh:
    fail()
if len(access.encode("utf-8")) > MAX_TOKEN_BYTES or len(refresh.encode("utf-8")) > MAX_TOKEN_BYTES:
    fail()
if any(ord(char) < 32 or 0x7f <= ord(char) <= 0x9f for char in access + refresh):
    fail()

sys.stdout.write(json.dumps(
    {"version": 1, "access_token": access, "refresh_token": refresh},
    ensure_ascii=True,
    separators=(",", ":"),
))
'
}

if [[ "$local_receiver" == true ]]; then
  if ! run_source_rpc | validate_rpc_json | run_local_receiver; then
    die 'token transfer failed; existing Hub credential was preserved'
  fi
elif [[ "$mac_keychain_receiver" == true ]]; then
  if ! run_source_rpc | validate_rpc_json | run_mac_keychain_receiver; then
    die 'token transfer failed; existing Hub credential was preserved'
  fi
else
  if ! run_source_rpc | validate_rpc_json | run_remote_receiver; then
    die 'token transfer failed; existing Hub credential was preserved'
  fi
fi

printf '%s\n' '{"status":"teslamate_rpc_tokens_activated"}'
