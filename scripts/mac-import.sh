#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

readonly APP_ROOT="${TESLATLAS_HUB_MAC_ROOT:-$HOME/Library/Application Support/Teslatlas Hub}"
readonly WRAPPER="$APP_ROOT/bin/teslatlas-hub-service"
readonly SUPERVISED="$APP_ROOT/bin/teslatlas-hub-supervised"
readonly SELF_PATH="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/$(basename -- "${BASH_SOURCE[0]}")"

[[ -x "$WRAPPER" ]] || {
  printf '%s\n' "Teslatlas Hub macOS installation is incomplete" >&2
  exit 1
}

lock_held_mode=false
supervised_was_active_handoff=false
lock_handoff=''
while (($#)); do
  case "$1" in
    --compatibility-lock-held)
      [[ "$lock_held_mode" != true ]] || {
        printf '%s\n' "--compatibility-lock-held may be specified only once" >&2
        exit 2
      }
      lock_held_mode=true
      shift
      ;;
    --mac-supervised-was-active)
      [[ "$lock_held_mode" == true && "$supervised_was_active_handoff" != true ]] || {
        printf '%s\n' "invalid internal supervised-state handoff" >&2
        exit 2
      }
      supervised_was_active_handoff=true
      shift
      ;;
    --mac-lock-handoff)
      [[ "$lock_held_mode" == true && $# -ge 2 && -z "$lock_handoff" && "$2" == /* ]] || {
        printf '%s\n' "invalid internal compatibility-lock handoff" >&2
        exit 2
      }
      lock_handoff=$2
      shift 2
      ;;
    *) break ;;
  esac
done
original_args=("$@")
car_id_args=()
car_id_seen=0
preflight=0
while (($#)); do
  case "$1" in
    --preflight)
      ((preflight == 0)) || {
        printf '%s\n' "--preflight may be specified only once" >&2
        exit 2
      }
      preflight=1
      shift
      ;;
    --car-id)
      (($# >= 2)) || {
        printf '%s\n' "--car-id requires one positive integer" >&2
        exit 2
      }
      ((car_id_seen == 0)) || {
        printf '%s\n' "--car-id may be specified only once" >&2
        exit 2
      }
      [[ "$2" =~ ^[1-9][0-9]*$ ]] || {
        printf '%s\n' "--car-id must be a positive integer" >&2
        exit 2
      }
      car_id_args=(--car-id "$2")
      car_id_seen=1
      shift 2
      ;;
    --help|-h)
      printf '%s\n' "usage: teslatlas-hub-import [--car-id POSITIVE_INTEGER] [--preflight --car-id POSITIVE_INTEGER]"
      exit 0
      ;;
    *)
      printf '%s\n' "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if ((preflight)); then
  ((car_id_seen)) || {
    printf '%s\n' "--preflight requires --car-id" >&2
    exit 2
  }
  exec "$WRAPPER" preflight-tesla-mate "${car_id_args[@]}"
fi

[[ -x "$SUPERVISED" ]] || {
  printf '%s\n' "Teslatlas Hub macOS supervised controller is missing" >&2
  exit 1
}

supervised_was_active=0
supervised_restored=false
handoff_dir=''
if [[ "$supervised_was_active_handoff" == true ]]; then
  supervised_was_active=1
fi
mac_supervised_state() {
  local state
  state="$("$SUPERVISED" active-state)" || return 1
  case "$state" in
    active|inactive)
      printf '%s\n' "$state"
      ;;
    *) return 1 ;;
  esac
}
record_lock_handoff() {
  local state=$1
  [[ -n "$lock_handoff" ]] || return 0
  [[ -f "$lock_handoff" && ! -L "$lock_handoff" && -O "$lock_handoff" ]] || return 1
  printf '%s\n' "$state" >"$lock_handoff"
}
lock_handoff_restored() {
  local path=$1
  local state
  [[ -f "$path" && ! -L "$path" && -O "$path" ]] || return 1
  IFS= read -r state <"$path" || return 1
  [[ "$state" == restored ]]
}
on_exit() {
  local status=$?
  local restored_state
  if ((supervised_was_active)) && [[ "$supervised_restored" != true ]]; then
    if ! "$SUPERVISED" enable >/dev/null 2>&1 ||
      ! restored_state="$(mac_supervised_state)" ||
      [[ "$restored_state" != active ]]; then
      printf '%s\n' "Teslatlas Hub supervised collection could not be restored" >&2
      ((status == 0)) && status=1
    else
      supervised_restored=true
    fi
  fi
  if [[ -n "$lock_handoff" ]]; then
    if ((supervised_was_active == 0)) || [[ "$supervised_restored" == true ]]; then
      if ! record_lock_handoff restored; then
        printf '%s\n' "Teslatlas Hub compatibility-lock restoration handoff failed" >&2
        ((status == 0)) && status=1
      fi
    else
      record_lock_handoff restore-failed || true
    fi
  fi
  if [[ -n "$handoff_dir" ]]; then
    rm -rf -- "$handoff_dir"
  fi
  exit "$status"
}
trap on_exit EXIT

if [[ "$lock_held_mode" == true ]]; then
  "$WRAPPER" verify-compatibility-lock || {
    printf '%s\n' "Teslatlas Hub compatibility lock lease is invalid" >&2
    exit 1
  }
fi
supervised_state="$(mac_supervised_state)" || {
  printf '%s\n' "Teslatlas Hub supervised collection state is unavailable" >&2
  exit 1
}
if [[ "$supervised_state" == active ]]; then
  supervised_was_active=1
  "$SUPERVISED" disable >/dev/null
  supervised_state="$(mac_supervised_state)" || {
    printf '%s\n' "Teslatlas Hub supervised collection state is unavailable after pause" >&2
    exit 1
  }
  if [[ "$supervised_state" != inactive ]]; then
    printf '%s\n' "Teslatlas Hub supervised collection could not be paused" >&2
    exit 1
  fi
fi

if [[ "$lock_held_mode" != true ]]; then
  handoff_dir="$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-import-lock.XXXXXX")" || exit 1
  chmod 700 "$handoff_dir" || exit 1
  lock_handoff="$handoff_dir/restoration"
  : >"$lock_handoff"
  chmod 600 "$lock_handoff" || exit 1
  child_args=("$SELF_PATH" --compatibility-lock-held --mac-lock-handoff "$lock_handoff")
  if ((supervised_was_active)); then
    child_args+=(--mac-supervised-was-active)
  fi
  child_args+=("${original_args[@]}")
  if "$WRAPPER" with-compatibility-lock -- "${child_args[@]}"; then
    child_status=0
  else
    child_status=$?
  fi
  if lock_handoff_restored "$lock_handoff"; then
    supervised_restored=true
  fi
  rm -rf -- "$handoff_dir"
  handoff_dir=''
  lock_handoff=''
  exit "$child_status"
fi

if "$WRAPPER" import-tesla-mate "${car_id_args[@]}"; then
  exit 0
else
  status=$?
  exit "$status"
fi
