#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

readonly APP_ROOT="${TESLATLAS_HUB_MAC_ROOT:-$HOME/Library/Application Support/Teslatlas Hub}"
readonly LABEL="com.teslatlas.hub"
readonly CONFIG="$APP_ROOT/config.toml"
readonly BIN="$APP_ROOT/bin/teslatlas-hub"

[[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]] || {
  printf '%s\n' "Apple-silicon macOS is required" >&2
  exit 1
}
[[ -x "$BIN" && -f "$CONFIG" ]] || {
  printf '%s\n' "Teslatlas Hub is not installed" >&2
  exit 1
}

launchctl print "gui/$(id -u)/$LABEL" |
  awk '/state = running/{found=1} END{exit !found}'
codesign --verify --strict "$BIN"
"$BIN" --config "$CONFIG" doctor >/dev/null

certificate="$(sed -n 's/^[[:space:]]*certificate_path[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "$CONFIG")"
endpoint="$(sed -n 's/^[[:space:]]*public_url[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "$CONFIG")"
[[ -f "$certificate" && -n "$endpoint" ]] || {
  printf '%s\n' "TLS configuration is incomplete" >&2
  exit 1
}
curl --silent --show-error --fail --cacert "$certificate" \
  "${endpoint%/}/readyz" >/dev/null

printf '%s\n' "Teslatlas Hub macOS install verified."
