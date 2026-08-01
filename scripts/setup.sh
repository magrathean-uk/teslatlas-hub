#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
set +x
umask 077

readonly PROGRAM_NAME="${0##*/}"
readonly CONFIG_PATH="/etc/teslatlas/config.toml"
readonly CREDENTIAL_DIR="/etc/teslatlas/credentials"
readonly CURSOR_CREDENTIAL="$CREDENTIAL_DIR/cursor-key"

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

case "${1:-}" in
  -h|--help)
    exec /usr/bin/teslatlas-hub --config "$CONFIG_PATH" setup "$@"
    ;;
esac

no_start=false
for argument in "$@"; do
  [[ "$argument" == "--no-start" ]] && no_start=true
done

[[ "${EUID}" -eq 0 ]] || die "run as root, for example: sudo teslatlas-hub-setup"
command -v systemctl >/dev/null 2>&1 || die "systemd is required"
command -v systemd-creds >/dev/null 2>&1 || die "systemd-creds is required"
command -v systemd-run >/dev/null 2>&1 || die "systemd-run is required"

install -d -m 0700 -o root -g root "$CREDENTIAL_DIR"
if [[ ! -f "$CURSOR_CREDENTIAL" ]]; then
  temporary_credential="$(mktemp "$CREDENTIAL_DIR/.cursor-key.XXXXXX")"
  cleanup() {
    rm -f -- "$temporary_credential"
  }
  trap cleanup EXIT HUP INT TERM
  head -c 32 /dev/urandom |
    systemd-creds encrypt --name=cursor-key --with-key=host - - \
      >"$temporary_credential"
  chmod 0600 "$temporary_credential"
  chown root:root "$temporary_credential"
  mv -f -- "$temporary_credential" "$CURSOR_CREDENTIAL"
  trap - EXIT HUP INT TERM
fi

for unit in teslatlas-hub.service teslatlas-hub-collect.service teslatlas-hub-supervised.service teslatlas-hub-import@.service; do
  dropin_dir="/etc/systemd/system/$unit.d"
  install -d -m 0755 -o root -g root "$dropin_dir"
  cat >"$dropin_dir/10-cursor-key.conf" <<EOF
[Service]
LoadCredentialEncrypted=cursor-key:$CURSOR_CREDENTIAL
EOF
  chmod 0644 "$dropin_dir/10-cursor-key.conf"
done

mqtt_enabled=false
mqtt_username_credential=""
mqtt_password_credential=""
while IFS='=' read -r key value; do
  value="${value#\"}"
  value="${value%\"}"
  case "$key" in
    enabled) mqtt_enabled="$value" ;;
    username_credential) mqtt_username_credential="$value" ;;
    password_credential) mqtt_password_credential="$value" ;;
  esac
done < <(awk '
  /^[[:space:]]*\[mqtt\][[:space:]]*(#.*)?$/ { in_mqtt = 1; next }
  /^[[:space:]]*\[/ { in_mqtt = 0 }
  in_mqtt && /^[[:space:]]*(enabled|username_credential|password_credential)[[:space:]]*=/ {
    line = $0
    sub(/^[[:space:]]*/, "", line)
    split(line, parts, "=")
    key = parts[1]
    gsub(/[[:space:]]/, "", key)
    value = parts[2]
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    print key "=" value
  }
' "$CONFIG_PATH")

mqtt_dropin_dir="/etc/systemd/system/teslatlas-hub.service.d"
mqtt_dropin="$mqtt_dropin_dir/20-mqtt-credentials.conf"
if [[ "$mqtt_enabled" == true && ( -n "$mqtt_username_credential" || -n "$mqtt_password_credential" ) ]]; then
  [[ -n "$mqtt_username_credential" && -n "$mqtt_password_credential" ]] || \
    die "MQTT username_credential and password_credential must be configured together"
  [[ "$mqtt_username_credential" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid MQTT username credential name"
  [[ "$mqtt_password_credential" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid MQTT password credential name"
  for credential_name in "$mqtt_username_credential" "$mqtt_password_credential"; do
    credential_path="$CREDENTIAL_DIR/$credential_name"
    [[ -f "$credential_path" && ! -L "$credential_path" ]] || \
      die "MQTT credential file is missing: $credential_name"
  done
  install -d -m 0755 -o root -g root "$mqtt_dropin_dir"
  temporary_dropin="$(mktemp "$mqtt_dropin_dir/.20-mqtt-credentials.XXXXXX")"
  cat >"$temporary_dropin" <<EOF
[Service]
LoadCredentialEncrypted=$mqtt_username_credential:$CREDENTIAL_DIR/$mqtt_username_credential
LoadCredentialEncrypted=$mqtt_password_credential:$CREDENTIAL_DIR/$mqtt_password_credential
EOF
  chmod 0644 "$temporary_dropin"
  mv -f -- "$temporary_dropin" "$mqtt_dropin"
else
  rm -f -- "$mqtt_dropin"
fi
systemctl daemon-reload

publication_lock=/var/lib/teslatlas/.publication.lock
if [[ -e "$publication_lock" ]]; then
  [[ -f "$publication_lock" && ! -L "$publication_lock" ]] || \
    die "publication lock is not a regular file"
  chown teslatlas:teslatlas "$publication_lock"
  chmod 0600 "$publication_lock"
fi

systemd-run --pipe --wait --quiet \
  -p User=teslatlas \
  -p Group=teslatlas \
  /usr/bin/teslatlas-hub --config "$CONFIG_PATH" setup "$@"

chown -R root:teslatlas /etc/teslatlas/tls
find /etc/teslatlas/tls -type d -exec chmod 0750 {} +
find /etc/teslatlas/tls -type f -exec chmod 0640 {} +
if [[ "$no_start" == true ]]; then
  exit 0
fi
systemctl enable --now teslatlas-hub.service

exec systemd-run --pipe --wait --quiet \
  -p User=teslatlas \
  -p Group=teslatlas \
  -p "LoadCredentialEncrypted=cursor-key:$CURSOR_CREDENTIAL" \
  /usr/bin/teslatlas-hub --config "$CONFIG_PATH" pair
