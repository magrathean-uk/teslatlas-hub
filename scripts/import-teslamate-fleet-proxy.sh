#!/bin/bash
set -euo pipefail
set +x
umask 077

readonly CREDENTIALS_DIR=/etc/teslatlas/credentials
readonly OWNER_TOKEN_NAME=owner-token
readonly OWNER_TOKEN_PATH="$CREDENTIALS_DIR/$OWNER_TOKEN_NAME"
readonly LEGACY_TOKENS_PATH="$CREDENTIALS_DIR/teslamate-owner-tokens"
readonly CONFIG_PATH=/etc/teslatlas/config.toml
readonly RUNTIME_UNITS=(
  teslatlas-hub.service
  teslatlas-hub-collect.service
  teslatlas-hub-supervised.service
)

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: sudo teslatlas-hub-import-teslamate-fleet-proxy --teslamate-container NAME

Reads TeslaMate TOKEN and TESLA_API_HOST from Docker metadata, copies TOKEN
directly into a host-encrypted Hub credential, and configures Hub for that
provider endpoint. This supports TeslaMate provider-Fleet mode only.
EOF
}

container=''
while (($#)); do
  case "$1" in
    --teslamate-container)
      (($# >= 2)) || die 'missing value for --teslamate-container'
      container="$2"
      shift 2
      ;;
    -h|--help)
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
[[ -f "$CONFIG_PATH" && ! -L "$CONFIG_PATH" ]] || die 'Hub configuration is missing'
[[ ! -e "$LEGACY_TOKENS_PATH" ]] || \
  die 'legacy token pair already exists; credential modes cannot be mixed'
docker inspect "$container" >/dev/null 2>&1 || die 'TeslaMate container not found'

api_host="$(
  docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$container" |
    awk '
      BEGIN { matches = 0 }
      /^TESLA_API_HOST=/ {
        matches++
        if (matches == 1) {
          sub(/^TESLA_API_HOST=/, "")
          print
        }
      }
      END { if (matches != 1) exit 1 }
    '
)" || die 'TeslaMate TESLA_API_HOST is missing or ambiguous'
[[ "$api_host" =~ ^https://[A-Za-z0-9.-]+(:[1-9][0-9]{0,4})?/?$ ]] || \
  die 'TeslaMate TESLA_API_HOST is not a supported HTTPS origin'

install -d -o root -g root -m 0700 "$CREDENTIALS_DIR"
credential_tmp="$(mktemp "$CREDENTIALS_DIR/.${OWNER_TOKEN_NAME}.XXXXXX")"
config_tmp="$(mktemp /etc/teslatlas/.config.toml.XXXXXX)"
cleanup() {
  rm -f -- "$credential_tmp" "$config_tmp"
}
trap cleanup EXIT

docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$container" |
  awk '
    BEGIN { matches = 0; valid = 1 }
    /^TOKEN=/ {
      matches++
      if (matches == 1) {
        sub(/^TOKEN=/, "")
        sub(/^[[:space:]]+/, "")
        sub(/[[:space:]]+$/, "")
        sub(/^\?token=/, "")
        sub(/^token=/, "")
        if (length($0) == 0) valid = 0
        else printf "%s", $0
      } else valid = 0
    }
    END { if (matches != 1 || !valid) exit 1 }
  ' |
  systemd-creds encrypt --with-key=host --name="$OWNER_TOKEN_NAME" - "$credential_tmp" \
  || die 'TeslaMate provider token could not be copied as an encrypted credential'
chown root:root "$credential_tmp"
chmod 0600 "$credential_tmp"

awk -v api_host="$api_host" '
  function settings() {
    if (!written) {
      print "owner_api_base_url = \"" api_host "\""
      print "interval_seconds = 1"
      written = 1
    }
  }
  /^[[:space:]]*\[collector\][[:space:]]*(#.*)?$/ {
    print
    in_collector = 1
    found_collector = 1
    settings()
    next
  }
  /^[[:space:]]*\[/ {
    if (in_collector) settings()
    in_collector = 0
  }
  in_collector &&
    /^[[:space:]]*(owner_api_base_url|interval_seconds)[[:space:]]*=/ { next }
  { print }
  END {
    if (!found_collector) {
      print ""
      print "[collector]"
      settings()
    }
  }
' "$CONFIG_PATH" >"$config_tmp"
chown --reference="$CONFIG_PATH" "$config_tmp"
chmod --reference="$CONFIG_PATH" "$config_tmp"

mv -f -- "$credential_tmp" "$OWNER_TOKEN_PATH"
mv -f -- "$config_tmp" "$CONFIG_PATH"
trap - EXIT

for unit in "${RUNTIME_UNITS[@]}"; do
  dropin_dir="/etc/systemd/system/${unit}.d"
  install -d -o root -g root -m 0755 "$dropin_dir"
  dropin_tmp="$(mktemp "$dropin_dir/.10-${OWNER_TOKEN_NAME}.conf.XXXXXX")"
  printf '[Service]\nLoadCredentialEncrypted=%s:%s\n' \
    "$OWNER_TOKEN_NAME" "$OWNER_TOKEN_PATH" >"$dropin_tmp"
  chown root:root "$dropin_tmp"
  chmod 0644 "$dropin_tmp"
  mv -f -- "$dropin_tmp" "$dropin_dir/10-${OWNER_TOKEN_NAME}.conf"
done

systemctl daemon-reload
systemctl try-restart teslatlas-hub.service
printf '%s\n' 'TeslaMate provider-Fleet credential handoff finished.'
