#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

readonly APP_ROOT="${TESLATLAS_HUB_MAC_ROOT:-$HOME/Library/Application Support/Teslatlas Hub}"
readonly BIN="$APP_ROOT/bin/teslatlas-hub"
readonly KEYCHAIN="$APP_ROOT/bin/teslatlas-hub-keychain"
readonly CONFIG="$APP_ROOT/config.toml"
readonly ACCOUNT="$(id -un)"
readonly CURSOR_SERVICE="com.teslatlas.hub.cursor-key.v2"
readonly OWNER_SERVICE="com.teslatlas.hub.owner-tokens.v2"
readonly TESLAMATE_POSTGRES_PASSWORD_SERVICE="com.teslatlas.hub.teslamate-postgres-password.v1"
readonly COMPATIBILITY_LOCK="$APP_ROOT/.compatibility-collection.lock"
readonly LOCK_OWNER_FILE="$COMPATIBILITY_LOCK/owner"

[[ -x "$BIN" && -x "$KEYCHAIN" && -f "$CONFIG" ]] || {
  printf '%s\n' "Teslatlas Hub macOS installation is incomplete" >&2
  exit 1
}

# Nonsecret routing only. Rotated owner tokens travel to this helper on stdin.
export TESLATLAS_HUB_MAC_KEYCHAIN_HELPER="$KEYCHAIN"
export TESLATLAS_HUB_MAC_OWNER_SERVICE="$OWNER_SERVICE"
export TESLATLAS_HUB_MAC_ACCOUNT="$ACCOUNT"

runtime=''
child=''
lock_held=0
lock_owner_pid=''
lock_owner_start=''
lock_owner_lease=''

normalize_start_token() {
  local value=$1
  # macOS ps lstart is an ordinary C-locale timestamp such as
  # "Tue Jul 28 21:17:23 2026". Store and compare one canonical form so the
  # owner-file validation accepts neither punctuation nor locale ambiguity.
  value="$(printf '%s' "$value" | tr -d '[:space:]:')"
  [[ "$value" =~ ^[A-Z][a-z]{2}[A-Z][a-z]{2}[0-9]{1,2}[0-9]{6}[0-9]{4}$ ]] || return 1
  printf '%s\n' "$value"
}

process_start_token() {
  local pid=$1
  local value
  value="$(LC_ALL=C ps -p "$pid" -o lstart= 2>/dev/null)" || return 1
  normalize_start_token "$value"
}

new_lock_lease() {
  local value
  value="$(uuidgen 2>/dev/null)" || return 1
  [[ "$value" =~ ^[0-9A-Fa-f-]+$ ]] || return 1
  printf '%s\n' "$value"
}

read_lock_owner() {
  local line
  local -a fields
  fields=()
  [[ -f "$LOCK_OWNER_FILE" && ! -L "$LOCK_OWNER_FILE" ]] || return 1
  while IFS= read -r line || [[ -n "$line" ]]; do
    fields+=("$line")
  done <"$LOCK_OWNER_FILE"
  ((${#fields[@]} == 3)) || return 1
  [[ "${fields[0]}" =~ ^[1-9][0-9]*$ ]] || return 1
  [[ "${fields[2]}" =~ ^[0-9A-Fa-f-]+$ ]] || return 1
  lock_owner_pid=${fields[0]}
  lock_owner_start="$(normalize_start_token "${fields[1]}")" || return 1
  lock_owner_lease=${fields[2]}
}

lock_owner_state() {
  local actual_start
  if ! read_lock_owner; then
    printf '%s\n' uncertain
    return
  fi
  if actual_start="$(process_start_token "$lock_owner_pid")"; then
    if [[ "$actual_start" == "$lock_owner_start" ]]; then
      printf '%s\n' active
    else
      printf '%s\n' stale
    fi
    return
  fi
  if kill -0 "$lock_owner_pid" 2>/dev/null; then
    printf '%s\n' uncertain
  else
    printf '%s\n' stale
  fi
}

owner_is_ancestor() {
  local expected_pid=$1
  local current_pid=$$
  local parent_pid
  local depth=0
  while ((depth < 32)); do
    parent_pid="$(ps -p "$current_pid" -o ppid= 2>/dev/null | tr -d '[:space:]')"
    [[ "$parent_pid" =~ ^[0-9]+$ ]] || return 1
    [[ "$parent_pid" == "$expected_pid" ]] && return 0
    [[ "$parent_pid" == 1 || "$parent_pid" == 0 ]] && return 1
    current_pid=$parent_pid
    depth=$((depth + 1))
  done
  return 1
}

inherited_lock_is_valid() {
  local actual_start
  local inherited_pid="${TESLATLAS_HUB_COMPATIBILITY_LOCK_OWNER_PID:-}"
  local inherited_start="${TESLATLAS_HUB_COMPATIBILITY_LOCK_OWNER_START:-}"
  local inherited_lease="${TESLATLAS_HUB_COMPATIBILITY_LOCK_LEASE:-}"
  [[ "$inherited_pid" =~ ^[1-9][0-9]*$ ]] || return 1
  [[ "$inherited_start" =~ ^[A-Za-z0-9]+$ ]] || return 1
  [[ "$inherited_lease" =~ ^[0-9A-Fa-f-]+$ ]] || return 1
  read_lock_owner || return 1
  [[ "$lock_owner_pid" == "$inherited_pid" ]] || return 1
  [[ "$lock_owner_start" == "$inherited_start" ]] || return 1
  [[ "$lock_owner_lease" == "$inherited_lease" ]] || return 1
  actual_start="$(process_start_token "$lock_owner_pid")" || return 1
  [[ "$actual_start" == "$lock_owner_start" ]] || return 1
  owner_is_ancestor "$lock_owner_pid"
}

reclaim_stale_lock() {
  local retired_lock="${COMPATIBILITY_LOCK}.retired.${lock_owner_lease}"
  [[ ! -e "$retired_lock" ]] || return 1
  mv "$COMPATIBILITY_LOCK" "$retired_lock" 2>/dev/null || return 1
  rm -f "$retired_lock/owner"
  rmdir "$retired_lock" 2>/dev/null
}

acquire_compatibility_lock() {
  local state
  if [[ -n "${TESLATLAS_HUB_COMPATIBILITY_LOCK_OWNER_PID:-}${TESLATLAS_HUB_COMPATIBILITY_LOCK_OWNER_START:-}${TESLATLAS_HUB_COMPATIBILITY_LOCK_LEASE:-}" ]]; then
    inherited_lock_is_valid && return 0
    return 11
  fi
  if mkdir "$COMPATIBILITY_LOCK" 2>/dev/null; then
    lock_owner_pid=$$
    lock_owner_start="$(process_start_token "$$")" || {
      rmdir "$COMPATIBILITY_LOCK" 2>/dev/null || true
      return 11
    }
    lock_owner_lease="$(new_lock_lease)" || {
      rmdir "$COMPATIBILITY_LOCK" 2>/dev/null || true
      return 11
    }
    if ! printf '%s\n%s\n%s\n' "$lock_owner_pid" "$lock_owner_start" "$lock_owner_lease" >"$LOCK_OWNER_FILE"; then
      rmdir "$COMPATIBILITY_LOCK" 2>/dev/null || true
      return 11
    fi
    if ! chmod 600 "$LOCK_OWNER_FILE"; then
      rm -f "$LOCK_OWNER_FILE"
      rmdir "$COMPATIBILITY_LOCK" 2>/dev/null || true
      return 11
    fi
    lock_held=1
    return 0
  fi
  state="$(lock_owner_state)"
  case "$state" in
    active) return 10 ;;
    stale)
      reclaim_stale_lock || return 11
      acquire_compatibility_lock
      return
      ;;
    *) return 11 ;;
  esac
}

acquire_compatibility_lock_or_die() {
  local status
  if acquire_compatibility_lock; then
    return 0
  fi
  status=$?
  if ((status == 10)); then
    printf '%s\n' 'Teslatlas Hub import or compatibility collection is already running' >&2
  else
    printf '%s\n' 'Teslatlas Hub compatibility lock ownership is uncertain' >&2
  fi
  exit 1
}
release_compatibility_lock() {
  if ((lock_held)); then
    if read_lock_owner && [[ "$lock_owner_pid" == "$$" && "$lock_owner_start" == "$(process_start_token "$$")" ]]; then
      rm -f "$LOCK_OWNER_FILE"
      rmdir "$COMPATIBILITY_LOCK" 2>/dev/null || true
    fi
    lock_held=0
  fi
}
cleanup() {
  if [[ -n "$child" ]] && kill -0 "$child" 2>/dev/null; then
    kill "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
  fi
  if [[ -n "$runtime" ]]; then
    rm -rf -- "$runtime"
  fi
  release_compatibility_lock
}
trap cleanup EXIT HUP INT TERM

with_compatibility_lock() {
  local wait_for_lock=$1
  shift
  local status
  while true; do
    if acquire_compatibility_lock; then
      break
    else
      status=$?
    fi
    if [[ "$wait_for_lock" == true && "$status" -eq 10 ]]; then
      sleep 1
      continue
    fi
    if ((status == 10)); then
      printf '%s\n' 'Teslatlas Hub import or compatibility collection is already running' >&2
    else
      printf '%s\n' 'Teslatlas Hub compatibility lock ownership is uncertain' >&2
    fi
    return 1
  done
  export TESLATLAS_HUB_COMPATIBILITY_LOCK_OWNER_PID="$lock_owner_pid"
  export TESLATLAS_HUB_COMPATIBILITY_LOCK_OWNER_START="$lock_owner_start"
  export TESLATLAS_HUB_COMPATIBILITY_LOCK_LEASE="$lock_owner_lease"
  "$@" &
  child=$!
  if wait "$child"; then
    status=0
  else
    status=$?
  fi
  child=''
  return "$status"
}

verify_compatibility_lock_child() {
  local child_pid=$1
  local parent_pid
  [[ "$child_pid" =~ ^[1-9][0-9]*$ ]] || return 1
  inherited_lock_is_valid || return 1
  parent_pid="$(ps -p "$child_pid" -o ppid= 2>/dev/null | tr -d '[:space:]')"
  [[ "$parent_pid" == "$lock_owner_pid" ]]
}

if [[ "${1:-}" == with-compatibility-lock ]]; then
  shift
  wait_for_lock=false
  if [[ "${1:-}" == --wait ]]; then
    wait_for_lock=true
    shift
  fi
  [[ "${1:-}" == -- && $# -ge 2 ]] || {
    printf '%s\n' 'usage: mac-service.sh with-compatibility-lock [--wait] -- COMMAND [ARGS...]' >&2
    exit 2
  }
  shift
  if with_compatibility_lock "$wait_for_lock" "$@"; then
    exit 0
  fi
  exit $?
fi

if [[ "${1:-}" == verify-compatibility-lock-child ]]; then
  [[ $# -eq 2 ]] || exit 2
  verify_compatibility_lock_child "$2"
  exit $?
fi

if [[ "${1:-}" == verify-compatibility-lock ]]; then
  [[ $# -eq 1 ]] || exit 2
  inherited_lock_is_valid
  exit $?
fi

runtime="$(mktemp -d "${TMPDIR:-/tmp}/com.teslatlas.hub.credentials.XXXXXX")"

case "${1:-}" in
  import-tesla-mate|collect-once|collect-supervised)
    acquire_compatibility_lock_or_die
    ;;
esac

"$KEYCHAIN" get "$CURSOR_SERVICE" "$ACCOUNT" >"$runtime/cursor-key"
chmod 0600 "$runtime/cursor-key"
if "$KEYCHAIN" exists "$OWNER_SERVICE" "$ACCOUNT"; then
  "$KEYCHAIN" get "$OWNER_SERVICE" "$ACCOUNT" >"$runtime/teslamate-owner-tokens"
  chmod 0600 "$runtime/teslamate-owner-tokens"
fi

if [[ "${1:-}" == "preflight-tesla-mate" || "${1:-}" == "import-tesla-mate" ]]; then
  "$KEYCHAIN" exists "$TESLAMATE_POSTGRES_PASSWORD_SERVICE" "$ACCOUNT" || {
    printf '%s\n' "TeslaMate PostgreSQL password is not present in Keychain" >&2
    exit 1
  }
  "$KEYCHAIN" get "$TESLAMATE_POSTGRES_PASSWORD_SERVICE" "$ACCOUNT" >"$runtime/teslamate-postgres-password"
  chmod 0600 "$runtime/teslamate-postgres-password"
fi

if (($# == 0)); then
  set -- serve
fi
CREDENTIALS_DIRECTORY="$runtime" "$BIN" --config "$CONFIG" "$@" &
child=$!
wait "$child"
status=$?
child=''
exit "$status"
