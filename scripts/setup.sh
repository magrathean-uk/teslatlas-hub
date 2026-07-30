#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
set +x
umask 077

readonly PROGRAM_NAME="${0##*/}"
readonly CONFIG_PATH="/etc/teslatlas/config.toml"

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

[[ "${EUID}" -eq 0 ]] || die "run as root, for example: sudo teslatlas-hub-setup"
command -v systemctl >/dev/null 2>&1 || die "systemd is required"
command -v runuser >/dev/null 2>&1 || die "runuser is required"

/usr/bin/teslatlas-hub --config "$CONFIG_PATH" setup "$@"

exec runuser -u teslatlas -- \
  /usr/bin/teslatlas-hub --config "$CONFIG_PATH" pair
