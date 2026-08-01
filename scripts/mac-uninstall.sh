#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

readonly APP_ROOT="${TESLATLAS_HUB_MAC_ROOT:-$HOME/Library/Application Support/Teslatlas Hub}"
readonly PLIST="$HOME/Library/LaunchAgents/com.teslatlas.hub.plist"
readonly SUPERVISED_PLIST="$HOME/Library/LaunchAgents/com.teslatlas.hub.supervised.plist"
readonly LABEL="com.teslatlas.hub"
readonly SUPERVISED_LABEL="com.teslatlas.hub.supervised"

[[ "$(uname -s)" == "Darwin" ]] || {
  printf '%s\n' "macOS is required" >&2
  exit 1
}

launchctl bootout "gui/$(id -u)/$LABEL" >/dev/null 2>&1 || true
launchctl bootout "gui/$(id -u)/$SUPERVISED_LABEL" >/dev/null 2>&1 || true
if [[ -d "$APP_ROOT/bin" ]]; then
  find "$APP_ROOT/bin" -type f -delete
  rmdir "$APP_ROOT/bin"
fi
if [[ -d "$APP_ROOT/share" ]]; then
  find "$APP_ROOT/share" -type f -delete
  rmdir "$APP_ROOT/share"
fi
if [[ -f "$PLIST" ]]; then
  find "$PLIST" -type f -delete
fi
if [[ -f "$SUPERVISED_PLIST" ]]; then
  find "$SUPERVISED_PLIST" -type f -delete
fi

printf '%s\n' "Teslatlas Hub service removed. State, configuration, backups, logs, and Keychain credentials preserved."
