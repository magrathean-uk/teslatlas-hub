#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
APP_ROOT="${TESLATLAS_HUB_MAC_ROOT:-$HOME/Library/Application Support/Teslatlas Hub}"
LOG_ROOT="${TESLATLAS_HUB_MAC_LOG_ROOT:-$HOME/Library/Logs/Teslatlas Hub}"
PLIST="$HOME/Library/LaunchAgents/com.teslatlas.hub.plist"
LABEL="com.teslatlas.hub"
PORT="${TESLATLAS_HUB_MAC_PORT:-8443}"
LAN_ADDRESS="${TESLATLAS_HUB_MAC_LAN_ADDRESS:-}"
ACCOUNT="$(id -un)"
CURSOR_SERVICE="com.teslatlas.hub.cursor-key.v2"
LEGACY_CURSOR_SERVICE="com.teslatlas.hub.cursor-key"
OWNER_SERVICE="com.teslatlas.hub.owner-tokens.v2"
LEGACY_OWNER_SERVICE="com.teslatlas.hub.owner-tokens"
TESLAMATE_POSTGRES_PASSWORD_SERVICE="com.teslatlas.hub.teslamate-postgres-password.v1"

teslamate_password_stdin=0
while (($#)); do
  case "$1" in
    --teslamate-postgres-password-stdin)
      teslamate_password_stdin=1
      shift
      ;;
    *)
      printf '%s\n' "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

[[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]] || {
  printf '%s\n' "Apple-silicon macOS is required" >&2
  exit 1
}
command -v cargo >/dev/null 2>&1 || { printf '%s\n' "cargo is required" >&2; exit 1; }
command -v swiftc >/dev/null 2>&1 || { printf '%s\n' "swiftc is required" >&2; exit 1; }

if [[ -z "$LAN_ADDRESS" ]]; then
  interface="$(route -n get default 2>/dev/null | awk '/interface:/{print $2; exit}')"
  if [[ -n "$interface" ]]; then
    LAN_ADDRESS="$(ipconfig getifaddr "$interface" 2>/dev/null || true)"
  fi
fi
[[ -n "$LAN_ADDRESS" ]] || {
  printf '%s\n' "cannot detect a LAN address; set TESLATLAS_HUB_MAC_LAN_ADDRESS" >&2
  exit 1
}

mkdir -p "$APP_ROOT/bin" "$APP_ROOT/data" "$APP_ROOT/share" "$LOG_ROOT" "$HOME/Library/LaunchAgents"
chmod 0700 "$APP_ROOT" "$APP_ROOT/bin" "$APP_ROOT/data" "$APP_ROOT/share" "$LOG_ROOT"

cd "$ROOT"
cargo build --release
install -m 0755 target/release/teslatlas-hub "$APP_ROOT/bin/teslatlas-hub"
swiftc -O scripts/mac-keychain.swift -o "$APP_ROOT/bin/teslatlas-hub-keychain"
install -m 0755 scripts/mac-service.sh "$APP_ROOT/bin/teslatlas-hub-service"
install -m 0755 scripts/mac-import.sh "$APP_ROOT/bin/teslatlas-hub-import"
install -m 0755 scripts/mac-supervised.sh "$APP_ROOT/bin/teslatlas-hub-supervised"
install -m 0600 packaging/com.teslatlas.hub.supervised.plist.in \
  "$APP_ROOT/share/com.teslatlas.hub.supervised.plist.in"
codesign --force --sign - "$APP_ROOT/bin/teslatlas-hub"
codesign --force --sign - "$APP_ROOT/bin/teslatlas-hub-keychain"

KEYCHAIN="$APP_ROOT/bin/teslatlas-hub-keychain"

if ((teslamate_password_stdin)); then
  [[ ! -t 0 ]] || {
    printf '%s\n' "TeslaMate PostgreSQL password must be supplied on stdin, not typed as an argument" >&2
    exit 2
  }
  postgres_password="$(cat)"
  [[ -n "$postgres_password" ]] || {
    printf '%s\n' "TeslaMate PostgreSQL password cannot be empty" >&2
    exit 2
  }
  [[ "$postgres_password" != *$'\n'* && "$postgres_password" != *$'\r'* ]] || {
    printf '%s\n' "TeslaMate PostgreSQL password must not contain line breaks" >&2
    exit 2
  }
  password_bytes="$(LC_ALL=C printf '%s' "$postgres_password" | wc -c | tr -d '[:space:]')"
  ((password_bytes <= 4096)) || {
    printf '%s\n' "TeslaMate PostgreSQL password is too large" >&2
    exit 2
  }
  printf '%s' "$postgres_password" |
    "$KEYCHAIN" set "$TESLAMATE_POSTGRES_PASSWORD_SERVICE" "$ACCOUNT"
  unset postgres_password password_bytes
fi

run_with_timeout() {
  local seconds="$1"
  shift
  "$@" &
  local command_pid=$!
  (
    sleep "$seconds"
    if kill -0 "$command_pid" 2>/dev/null; then
      kill -TERM "$command_pid" 2>/dev/null || true
      sleep 0.2
      kill -KILL "$command_pid" 2>/dev/null || true
    fi
  ) &
  local watchdog_pid=$!
  local status
  if wait "$command_pid"; then
    status=0
  else
    status=$?
  fi
  kill "$watchdog_pid" 2>/dev/null || true
  wait "$watchdog_pid" 2>/dev/null || true
  return "$status"
}

migrate_legacy_owner_tokens() {
  "$KEYCHAIN" exists "$OWNER_SERVICE" "$ACCOUNT" && return 0
  "$KEYCHAIN" exists "$LEGACY_OWNER_SERVICE" "$ACCOUNT" || return 0

  local candidate
  candidate="$(mktemp "${TMPDIR:-/tmp}/com.teslatlas.hub.owner-tokens.XXXXXX")"
  chmod 0600 "$candidate"
  if ! run_with_timeout 5 "$KEYCHAIN" get "$LEGACY_OWNER_SERVICE" "$ACCOUNT" >"$candidate"; then
    rm -f -- "$candidate"
    printf '%s\n' "cannot migrate the legacy owner token from Keychain" >&2
    return 1
  fi
  if [[ ! -s "$candidate" ]] || ! "$KEYCHAIN" set "$OWNER_SERVICE" "$ACCOUNT" <"$candidate"; then
    rm -f -- "$candidate"
    printf '%s\n' "cannot store the migrated owner token in Keychain" >&2
    return 1
  fi
  rm -f -- "$candidate"
}

migrate_legacy_cursor_key() {
  "$KEYCHAIN" exists "$CURSOR_SERVICE" "$ACCOUNT" && return 0
  if ! "$KEYCHAIN" exists "$LEGACY_CURSOR_SERVICE" "$ACCOUNT"; then
    openssl rand 32 | "$KEYCHAIN" set "$CURSOR_SERVICE" "$ACCOUNT"
    return 0
  fi

  local candidate decoded bytes hex_bytes other_bytes status
  candidate="$(mktemp "${TMPDIR:-/tmp}/com.teslatlas.hub.cursor-key.XXXXXX")"
  decoded="$(mktemp "${TMPDIR:-/tmp}/com.teslatlas.hub.cursor-key-decoded.XXXXXX")"
  chmod 0600 "$candidate" "$decoded"
  if ! run_with_timeout 5 security find-generic-password -s "$LEGACY_CURSOR_SERVICE" -a "$ACCOUNT" -w >"$candidate" 2>/dev/null; then
    rm -f -- "$candidate" "$decoded"
    printf '%s\n' "cannot migrate the legacy cursor key from Keychain" >&2
    return 1
  fi

  bytes="$(wc -c <"$candidate" | tr -d '[:space:]')"
  hex_bytes="$(LC_ALL=C tr -cd '0123456789abcdefABCDEF' <"$candidate" | wc -c | tr -d '[:space:]')"
  other_bytes="$(LC_ALL=C tr -d '0123456789abcdefABCDEF\r\n' <"$candidate" | wc -c | tr -d '[:space:]')"
  if [[ "$hex_bytes" == 64 && "$other_bytes" == 0 ]]; then
    xxd -r -p <"$candidate" >"$decoded"
    bytes="$(wc -c <"$decoded" | tr -d '[:space:]')"
    [[ "$bytes" == 32 ]] || status=1
    if [[ "${status:-0}" == 0 ]]; then
      "$KEYCHAIN" set "$CURSOR_SERVICE" "$ACCOUNT" <"$decoded" || status=1
    fi
  elif [[ "$bytes" == 32 ]]; then
    "$KEYCHAIN" set "$CURSOR_SERVICE" "$ACCOUNT" <"$candidate" || status=1
  else
    status=1
  fi
  rm -f -- "$candidate" "$decoded"
  if [[ "${status:-0}" != 0 ]]; then
    printf '%s\n' "legacy cursor key has an unsupported format" >&2
    return 1
  fi
}

migrate_legacy_cursor_key
migrate_legacy_owner_tokens

if [[ ! -f "$APP_ROOT/config.toml" ]]; then
  cat >"$APP_ROOT/config.toml" <<EOF
data_dir = "$APP_ROOT/data"
bind = "127.0.0.1:$PORT"
EOF
fi
chmod 0600 "$APP_ROOT/config.toml"

"$APP_ROOT/bin/teslatlas-hub" --config "$APP_ROOT/config.toml" setup \
  --lan-address "$LAN_ADDRESS" --port "$PORT" --no-start >/dev/null

escape_xml() {
  printf '%s' "$1" |
    sed -e 's/&/\\&amp;/g' -e 's/</\\&lt;/g' -e 's/>/\\&gt;/g'
}
wrapper_xml="$(escape_xml "$APP_ROOT/bin/teslatlas-hub-service")"
app_xml="$(escape_xml "$APP_ROOT")"
log_xml="$(escape_xml "$LOG_ROOT")"
sed \
  -e "s|@SERVICE_WRAPPER@|$wrapper_xml|g" \
  -e "s|@APP_ROOT@|$app_xml|g" \
  -e "s|@LOG_ROOT@|$log_xml|g" \
  packaging/com.teslatlas.hub.plist.in >"$PLIST"
chmod 0600 "$PLIST"
plutil -lint "$PLIST" >/dev/null

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 ||
  launchctl bootout "gui/$(id -u)/$LABEL" >/dev/null 2>&1 ||
  true
for _ in $(seq 1 50); do
  if ! launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
launchctl bootstrap "gui/$(id -u)" "$PLIST"
launchctl kickstart -k "gui/$(id -u)/$LABEL"

ready=0
for _ in $(seq 1 60); do
  if "$ROOT/scripts/mac-verify.sh" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 1
done
[[ "$ready" -eq 1 ]] || {
  printf '%s\n' "Teslatlas Hub did not become ready within 60 seconds" >&2
  exit 1
}

printf '%s\n' "Teslatlas Hub macOS service installed."
printf '%s\n' "Verify: $ROOT/scripts/mac-verify.sh"
