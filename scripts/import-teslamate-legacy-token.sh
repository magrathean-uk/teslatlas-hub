#!/usr/bin/env bash
set -euo pipefail

readonly CREDENTIALS_DIR=/etc/teslatlas/credentials
readonly ENCRYPTION_KEY_NAME=teslamate-encryption-key
readonly ENCRYPTION_KEY_PATH="$CREDENTIALS_DIR/$ENCRYPTION_KEY_NAME"
readonly OWNER_TOKENS_NAME=teslamate-owner-tokens
readonly OWNER_TOKENS_PATH="$CREDENTIALS_DIR/$OWNER_TOKENS_NAME"
readonly PREVIOUS_OWNER_TOKENS_NAME=teslamate-owner-tokens-previous
readonly PREVIOUS_OWNER_TOKENS_PATH="$CREDENTIALS_DIR/$PREVIOUS_OWNER_TOKENS_NAME"
readonly POSTGRES_PASSWORD_PATH="$CREDENTIALS_DIR/teslamate-postgres-password"
readonly DROP_IN_DIR=/etc/systemd/system/teslatlas-hub-token-import.service.d
readonly DROP_IN_PATH="$DROP_IN_DIR/10-teslamate-source-credentials.conf"
readonly TOKEN_IMPORT_UNIT=teslatlas-hub-token-import.service

die() {
  printf '%s\n' "error: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: sudo import-teslamate-legacy-token --teslamate-container NAME

Copies only TeslaMate's ENCRYPTION_KEY from the named container's read-only
metadata into a host-encrypted systemd credential, then runs the Hub's explicit
read-only legacy-token import. TeslaMate, Docker, and PostgreSQL are never
started, stopped, or changed.
EOF
}

container=''
while (($# > 0)); do
  case "$1" in
    --teslamate-container)
      (($# >= 2)) || die 'missing value for --teslamate-container'
      container=$2
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ $(id -u) -eq 0 ]] || die 'run as root'
[[ -n "$container" ]] || die '--teslamate-container is required'
command -v docker >/dev/null 2>&1 || die 'docker is required'
command -v systemctl >/dev/null 2>&1 || die 'systemctl is required'
command -v systemd-creds >/dev/null 2>&1 || die 'systemd-creds is required'
docker inspect "$container" >/dev/null 2>&1 || die 'TeslaMate container not found'
[[ -f "$POSTGRES_PASSWORD_PATH" ]] || die 'missing encrypted TeslaMate PostgreSQL password credential'

install -d -o root -g root -m 0700 "$CREDENTIALS_DIR"
key_tmp=$(mktemp "$CREDENTIALS_DIR/.${ENCRYPTION_KEY_NAME}.XXXXXX")
drop_in_tmp=''
cleanup() {
  rm -f -- "$key_tmp"
  [[ -z "$drop_in_tmp" ]] || rm -f -- "$drop_in_tmp"
}
trap cleanup EXIT

# The value is never assigned to a shell variable, printed, logged, or stored
# in plaintext. The line has no trailing newline so the TeslaMate key bytes are
# passed to systemd-creds unchanged.
docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$container" \
  | awk '
      BEGIN { matches = 0; valid = 1 }
      /^ENCRYPTION_KEY=/ {
        matches++
        if (matches == 1) {
          sub(/^ENCRYPTION_KEY=/, "")
          if (length($0) == 0) valid = 0
          else printf "%s", $0
        } else valid = 0
      }
      END { if (matches != 1 || !valid) exit 1 }
    ' \
  | systemd-creds encrypt --with-key=host --name="$ENCRYPTION_KEY_NAME" - "$key_tmp" \
  || die 'TeslaMate ENCRYPTION_KEY could not be copied as an encrypted credential'

chown root:root "$key_tmp"
chmod 0600 "$key_tmp"
mv -f -- "$key_tmp" "$ENCRYPTION_KEY_PATH"

install -d -o root -g root -m 0755 "$DROP_IN_DIR"
drop_in_tmp=$(mktemp "$DROP_IN_DIR/.10-teslamate-source-credentials.conf.XXXXXX")
cat > "$drop_in_tmp" <<EOF
[Service]
LoadCredentialEncrypted=teslamate-postgres-password:$POSTGRES_PASSWORD_PATH
LoadCredentialEncrypted=$ENCRYPTION_KEY_NAME:$ENCRYPTION_KEY_PATH
EOF
chown root:root "$drop_in_tmp"
chmod 0644 "$drop_in_tmp"
mv -f -- "$drop_in_tmp" "$DROP_IN_PATH"
drop_in_tmp=''

systemctl daemon-reload
systemctl start --wait "$TOKEN_IMPORT_UNIT"
[[ -f "$OWNER_TOKENS_PATH" ]] || die 'Hub owner-token credential was not created'

for unit in teslatlas-hub.service teslatlas-hub-collect.service teslatlas-hub-supervised.service; do
  runtime_drop_in_dir="/etc/systemd/system/$unit.d"
  runtime_drop_in_path="$runtime_drop_in_dir/10-$OWNER_TOKENS_NAME.conf"
  install -d -o root -g root -m 0755 "$runtime_drop_in_dir"
  cat >"$runtime_drop_in_path" <<EOF
[Service]
LoadCredentialEncrypted=$OWNER_TOKENS_NAME:$OWNER_TOKENS_PATH
EOF
  chown root:root "$runtime_drop_in_path"
  chmod 0644 "$runtime_drop_in_path"
  rm -f -- "$runtime_drop_in_dir/10-$PREVIOUS_OWNER_TOKENS_NAME.conf"
done

systemctl daemon-reload
systemctl try-restart teslatlas-hub.service
printf '%s\n' 'TeslaMate legacy token import finished.'
