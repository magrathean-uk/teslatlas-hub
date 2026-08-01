#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

readonly APP_ROOT="${TESLATLAS_HUB_MAC_ROOT:-$HOME/Library/Application Support/Teslatlas Hub}"
readonly LOG_ROOT="${TESLATLAS_HUB_MAC_LOG_ROOT:-$HOME/Library/Logs/Teslatlas Hub}"
readonly PLIST="$HOME/Library/LaunchAgents/com.teslatlas.hub.supervised.plist"
readonly LABEL="com.teslatlas.hub.supervised"
readonly TEMPLATE="$APP_ROOT/share/com.teslatlas.hub.supervised.plist.in"
readonly WRAPPER="$APP_ROOT/bin/teslatlas-hub-service"
readonly KEYCHAIN="$APP_ROOT/bin/teslatlas-hub-keychain"
readonly ACCOUNT="$(id -un)"
readonly OWNER_SERVICE="com.teslatlas.hub.owner-tokens.v2"
readonly CONFIG="$APP_ROOT/config.toml"
readonly LOCK_WRAPPER="$APP_ROOT/bin/teslatlas-hub-compatibility-wrapper"
readonly LAUNCH_DOMAIN="gui/$(id -u)"
readonly SELF_PATH="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/$(basename -- "${BASH_SOURCE[0]}")"

lock_held=false
if [[ "${1:-}" == --compatibility-lock-held ]]; then
  lock_held=true
  shift
fi

launchd_active_state() {
  local output
  if output="$(launchctl print "$LAUNCH_DOMAIN/$LABEL" 2>&1)"; then
    printf '%s\n' active
    return 0
  fi
  case "$output" in
    *'Could not find service'*|*'No such process'*)
      printf '%s\n' inactive
      return 0
      ;;
    *) return 1 ;;
  esac
}

stop_supervised_service() {
  local state
  state="$(launchd_active_state)" || return 1
  [[ "$state" == inactive ]] && return 0
  launchctl bootout "$LAUNCH_DOMAIN/$LABEL" >/dev/null 2>&1 || return 1
  state="$(launchd_active_state)" || return 1
  [[ "$state" == inactive ]]
}

has_inherited_compatibility_lease() {
  [[ -n "${TESLATLAS_HUB_COMPATIBILITY_LOCK_OWNER_PID:-}" || \
     -n "${TESLATLAS_HUB_COMPATIBILITY_LOCK_OWNER_START:-}" || \
     -n "${TESLATLAS_HUB_COMPATIBILITY_LOCK_LEASE:-}" ]]
}

enter_transition_lease() {
  local state

  # A cutover/import child already has the outer lease. Validate it in place:
  # a nested acquisition would be unnecessary and may wait on its own parent.
  if [[ "$lock_held" == true ]] || has_inherited_compatibility_lease; then
    "$WRAPPER" verify-compatibility-lock
    return
  fi

  # A running supervised collector owns the central lease. Stop and prove it
  # inactive before attempting a fresh acquisition, otherwise disable/enable
  # would wait on the very service that it must stop. Any competing owner that
  # wins after this quiesce causes the acquisition below to fail closed.
  stop_supervised_service || return 1
  state="$(launchd_active_state)" || return 1
  [[ "$state" == inactive ]] || return 1

  exec "$WRAPPER" with-compatibility-lock -- \
    "$SELF_PATH" --compatibility-lock-held "$@"
}

write_lock_wrapper() {
  local temporary="${LOCK_WRAPPER}.tmp.$$"
  local wrapper_literal
  wrapper_literal="$(printf '%q' "$WRAPPER")"
  {
    printf '%s\n' '#!/usr/bin/env bash' 'set -Eeuo pipefail' 'umask 077'
    printf 'readonly REAL_WRAPPER=%s\n' "$wrapper_literal"
    cat <<'EOF'
exec "$REAL_WRAPPER" with-compatibility-lock --wait -- "$REAL_WRAPPER" "$@"
EOF
  } >"$temporary"
  chmod 0700 "$temporary"
  mv -f -- "$temporary" "$LOCK_WRAPPER"
}

command="${1:-status}"
case "$command" in
  enable|disable)
    enter_transition_lease "$@" || {
      printf '%s\n' "Teslatlas Hub supervised transition could not establish a compatibility lease" >&2
      exit 1
    }
    ;;
esac

case "$command" in
  enable)
    [[ -x "$WRAPPER" && -x "$KEYCHAIN" && -f "$TEMPLATE" && -f "$CONFIG" ]] || {
      printf '%s\n' "Teslatlas Hub macOS installation is incomplete" >&2
      exit 1
    }
    interval="$(
      sed -n 's/^[[:space:]]*interval_seconds[[:space:]]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
        "$CONFIG" | tail -n1
    )"
    [[ -n "$interval" && "$interval" -gt 0 ]] || {
      printf '%s\n' "collector interval_seconds must be greater than zero" >&2
      exit 1
    }
    "$KEYCHAIN" exists "$OWNER_SERVICE" "$ACCOUNT" || {
      printf '%s\n' "owner token is not present in Keychain" >&2
      exit 1
    }
    stop_supervised_service || {
      printf '%s\n' "Teslatlas Hub supervised collection could not be stopped" >&2
      exit 1
    }
    write_lock_wrapper
    escape_xml() {
      printf '%s' "$1" |
        sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
    }
    wrapper_xml="$(escape_xml "$LOCK_WRAPPER")"
    app_xml="$(escape_xml "$APP_ROOT")"
    log_xml="$(escape_xml "$LOG_ROOT")"
    sed \
      -e "s|@SERVICE_WRAPPER@|$wrapper_xml|g" \
      -e "s|@APP_ROOT@|$app_xml|g" \
      -e "s|@LOG_ROOT@|$log_xml|g" \
      "$TEMPLATE" >"$PLIST"
    chmod 0600 "$PLIST"
    plutil -lint "$PLIST" >/dev/null
    launchctl bootstrap "$LAUNCH_DOMAIN" "$PLIST"
    launchctl kickstart -k "$LAUNCH_DOMAIN/$LABEL"
    service_state="$(launchd_active_state)" || {
      printf '%s\n' "Teslatlas Hub supervised collection failed to remain active" >&2
      exit 1
    }
    [[ "$service_state" == active ]] || {
      printf '%s\n' "Teslatlas Hub supervised collection failed to remain active" >&2
      exit 1
    }
    printf '%s\n' "Teslatlas Hub supervised collection enabled."
    ;;
  disable)
    stop_supervised_service || {
      printf '%s\n' "Teslatlas Hub supervised collection could not be stopped" >&2
      exit 1
    }
    service_state="$(launchd_active_state)" || {
      printf '%s\n' "Teslatlas Hub supervised collection stop state is uncertain" >&2
      exit 1
    }
    [[ "$service_state" == inactive ]] || {
      printf '%s\n' "Teslatlas Hub supervised collection remained active" >&2
      exit 1
    }
    [[ ! -f "$PLIST" ]] || rm -f -- "$PLIST"
    printf '%s\n' "Teslatlas Hub supervised collection disabled."
    ;;
  status)
    launchctl print "gui/$(id -u)/$LABEL"
    ;;
  is-active)
    service_state="$(launchd_active_state)" || exit 1
    [[ "$service_state" == active ]]
    ;;
  active-state)
    launchd_active_state
    ;;
  *)
    printf '%s\n' "usage: mac-supervised.sh enable|disable|status|is-active|active-state" >&2
    exit 2
    ;;
esac
