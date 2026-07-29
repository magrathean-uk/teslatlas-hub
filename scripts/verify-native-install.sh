#!/usr/bin/env bash
# Read-only post-install verification for a Debian host.
set -euo pipefail
IFS=$'\n\t'
set +x

readonly PROGRAM_NAME="${0##*/}"
config="/etc/teslatlas/config.toml"
readonly OWNER_TOKEN_CREDENTIAL="owner-token"
readonly CURSOR_KEY_CREDENTIAL="cursor-key"
readonly TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL="teslamate-postgres-password"
readonly CREDENTIAL_DIRECTORY="/etc/teslatlas/credentials"
readonly OWNER_TOKEN_PATH="${CREDENTIAL_DIRECTORY}/${OWNER_TOKEN_CREDENTIAL}"
readonly CURSOR_KEY_PATH="${CREDENTIAL_DIRECTORY}/${CURSOR_KEY_CREDENTIAL}"
readonly TESLAMATE_POSTGRES_PASSWORD_PATH="${CREDENTIAL_DIRECTORY}/${TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL}"
readonly INSTALLED_SERVICES=("teslatlas-hub.service" "teslatlas-hub-collect.service" "teslatlas-hub-import@.service")
readonly CURSOR_KEY_SERVICES=("teslatlas-hub.service" "teslatlas-hub-collect.service" "teslatlas-hub-import@.service")
readonly OWNER_TOKEN_SERVICES=("teslatlas-hub.service" "teslatlas-hub-collect.service")
readonly TESLAMATE_PASSWORD_SERVICES=("teslatlas-hub-import@.service")

usage() {
  cat <<'EOF'
Usage: scripts/verify-native-install.sh [--config FILE]

Checks the installed Debian unit with systemd-analyze, confirms the service is
active, validates the Hub database, then checks /readyz for the default local
HTTP listener. A TLS listener is validated locally through `doctor`, preserving
its public hostname and certificate trust boundary. It makes no changes and
never reads credentials.
EOF
}

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

file_mode() {
  if stat -c '%a' -- "$1" >/dev/null 2>&1; then
    stat -c '%a' -- "$1"
  else
    stat -f '%Lp' "$1"
  fi
}

while (($#)); do
  case "$1" in
    --config)
      (($# >= 2)) || die "--config requires a file"
      config="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

command -v systemd-analyze >/dev/null 2>&1 || die "systemd-analyze is required"
command -v systemctl >/dev/null 2>&1 || die "systemctl is required"
for service in "${INSTALLED_SERVICES[@]}"; do
  [[ -r "/usr/lib/systemd/system/${service}" ]] || die "Hub systemd unit is not installed: ${service}"
  systemd-analyze verify "/usr/lib/systemd/system/${service}"
done
[[ -r "$config" ]] || die "Hub configuration is not readable"

systemctl is-active --quiet teslatlas-hub.service || die "teslatlas-hub.service is not active"

[[ -f "$CURSOR_KEY_PATH" && ! -L "$CURSOR_KEY_PATH" ]] || \
  die "cursor signing key credential is not a regular file"
cursor_key_mode="$(file_mode "$CURSOR_KEY_PATH")"
(( (8#$cursor_key_mode & 8#077) == 0 )) || \
  die "cursor signing key credential is group/world accessible"
for service in "${CURSOR_KEY_SERVICES[@]}"; do
  dropin_path="/etc/systemd/system/${service}.d/10-cursor-key.conf"
  [[ -f "$dropin_path" ]] || die "cursor signing key exists without its service drop-in"
  grep -Fqx \
    "LoadCredentialEncrypted=${CURSOR_KEY_CREDENTIAL}:${CURSOR_KEY_PATH}" \
    "$dropin_path" || die "cursor signing key drop-in is malformed"
done

if [[ -e "$OWNER_TOKEN_PATH" ]]; then
  [[ -f "$OWNER_TOKEN_PATH" && ! -L "$OWNER_TOKEN_PATH" ]] || \
    die "owner credential is not a regular file"
  owner_token_mode="$(file_mode "$OWNER_TOKEN_PATH")"
  (( (8#$owner_token_mode & 8#077) == 0 )) || \
    die "owner credential is group/world accessible"
  for service in "${OWNER_TOKEN_SERVICES[@]}"; do
    dropin_path="/etc/systemd/system/${service}.d/10-owner-token.conf"
    [[ -f "$dropin_path" ]] || die "owner credential exists without its service drop-in"
    grep -Fqx \
      "LoadCredentialEncrypted=${OWNER_TOKEN_CREDENTIAL}:${OWNER_TOKEN_PATH}" \
      "$dropin_path" || die "owner credential drop-in is malformed"
  done
else
  for service in "${OWNER_TOKEN_SERVICES[@]}"; do
    dropin_path="/etc/systemd/system/${service}.d/10-owner-token.conf"
    [[ ! -e "$dropin_path" ]] || die "owner credential drop-in exists without encrypted credential"
  done
fi

if [[ -e "$TESLAMATE_POSTGRES_PASSWORD_PATH" ]]; then
  [[ -f "$TESLAMATE_POSTGRES_PASSWORD_PATH" && ! -L "$TESLAMATE_POSTGRES_PASSWORD_PATH" ]] || \
    die "TeslaMate PostgreSQL password credential is not a regular file"
  postgres_password_mode="$(file_mode "$TESLAMATE_POSTGRES_PASSWORD_PATH")"
  (( (8#$postgres_password_mode & 8#077) == 0 )) || \
    die "TeslaMate PostgreSQL password credential is group/world accessible"
  for service in "${TESLAMATE_PASSWORD_SERVICES[@]}"; do
    dropin_path="/etc/systemd/system/${service}.d/10-${TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL}.conf"
    [[ -f "$dropin_path" ]] || die "TeslaMate PostgreSQL password exists without its service drop-in"
    grep -Fqx \
      "LoadCredentialEncrypted=${TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL}:${TESLAMATE_POSTGRES_PASSWORD_PATH}" \
      "$dropin_path" || die "TeslaMate PostgreSQL password drop-in is malformed"
  done
else
  for service in "${TESLAMATE_PASSWORD_SERVICES[@]}"; do
    dropin_path="/etc/systemd/system/${service}.d/10-${TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL}.conf"
    [[ ! -e "$dropin_path" ]] || die "TeslaMate PostgreSQL password drop-in exists without encrypted credential"
  done
fi

config_uses_tls() {
  awk '
    /^[[:space:]]*\[tls\][[:space:]]*(#.*)?$/ { found = 1 }
    END { exit !found }
  ' "$config"
}

if command -v curl >/dev/null 2>&1 && ! config_uses_tls; then
  configured_bind="$(awk -F'"' '/^[[:space:]]*bind[[:space:]]*=[[:space:]]*"/ { print $2; exit }' "$config")"
  [[ -n "$configured_bind" ]] || die "cannot determine configured bind address"
  curl --fail --silent --show-error --max-time 5 "http://${configured_bind}/readyz" >/dev/null
else
  /usr/bin/teslatlas-hub --config "$config" doctor >/dev/null
fi

printf '%s\n' "Teslatlas Hub native install verified."
