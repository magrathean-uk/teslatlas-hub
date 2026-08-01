#!/usr/bin/env bash
# Proves systemd-creds embedded-name handling on a Debian systemd host.
set -euo pipefail
umask 077

command -v systemd-creds >/dev/null 2>&1 || { printf '%s\n' 'SKIP: systemd-creds is unavailable' >&2; exit 77; }
command -v systemd-run >/dev/null 2>&1 || { printf '%s\n' 'SKIP: systemd-run is unavailable' >&2; exit 77; }
[[ "$(id -u)" == 0 ]] || { printf '%s\n' 'SKIP: root is required' >&2; exit 77; }
[[ -d /run/systemd/system ]] || { printf '%s\n' 'SKIP: systemd system manager is unavailable' >&2; exit 77; }

temp_dir=$(mktemp -d /tmp/teslatlas-systemd-creds.XXXXXX)
trap 'rm -rf -- "$temp_dir"' EXIT
current="$temp_dir/teslamate-owner-tokens"
previous="$temp_dir/teslamate-owner-tokens-previous"
marker="$temp_dir/activated"
input="$temp_dir/input.json"
printf '%s' '{"version":1,"access_token":"fixture-access","refresh_token":"fixture-refresh"}' >"$input"

systemd-creds encrypt --with-key=host --name=teslamate-owner-tokens "$input" "$current" >/dev/null
cp -- "$current" "$previous"
chmod 600 "$current" "$previous"

unit="teslatlas-hub-credential-fixture-$$"
cleanup_unit() {
  systemctl reset-failed "$unit.service" >/dev/null 2>&1 || true
  systemctl stop "$unit.service" >/dev/null 2>&1 || true
}
trap 'cleanup_unit; rm -rf -- "$temp_dir"' EXIT

# The rollback file is present, but only the current path is loaded. This must work.
systemd-run --quiet --wait --collect --unit="$unit" \
  --property="LoadCredentialEncrypted=teslamate-owner-tokens:$current" \
  /bin/sh -c 'test -f "$CREDENTIALS_DIRECTORY/teslamate-owner-tokens" && : > "$1"' sh "$marker"
test -f "$marker"

# Loading the same ciphertext under the previous alias must fail: the embedded
# name remains teslamate-owner-tokens. Do not print systemd-creds/systemd-run output.
if systemd-run --quiet --wait --collect --unit="${unit}-wrong" \
  --property="LoadCredentialEncrypted=teslamate-owner-tokens-previous:$previous" \
  /bin/true >/dev/null 2>&1; then
  printf '%s\n' 'error: systemd accepted an embedded-name mismatch' >&2
  exit 1
fi

printf '%s\n' 'systemd credential current-only activation passed'
