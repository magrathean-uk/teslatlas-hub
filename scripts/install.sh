#!/usr/bin/env bash
# Teslatlas Hub native bootstrap.
#
# This script deliberately has no --token option. Tokens enter only through a
# protected input file or a password-agent pipe, then go straight into a
# host-encrypted systemd credential.
set -euo pipefail
IFS=$'\n\t'
set +x
umask 077

readonly PROGRAM_NAME="${0##*/}"
readonly DEFAULT_MIN_FREE_KIB=$((2 * 1024 * 1024))
readonly OWNER_TOKEN_CREDENTIAL="owner-token"
readonly CURSOR_KEY_CREDENTIAL="cursor-key"
readonly TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL="teslamate-postgres-password"
readonly CREDENTIAL_DIRECTORY="/etc/teslatlas/credentials"
readonly OWNER_TOKEN_PATH="${CREDENTIAL_DIRECTORY}/${OWNER_TOKEN_CREDENTIAL}"
readonly CURSOR_KEY_PATH="${CREDENTIAL_DIRECTORY}/${CURSOR_KEY_CREDENTIAL}"
readonly TESLAMATE_POSTGRES_PASSWORD_PATH="${CREDENTIAL_DIRECTORY}/${TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL}"
readonly CURSOR_KEY_SERVICES=("teslatlas-hub.service" "teslatlas-hub-collect.service" "teslatlas-hub-import@.service")
readonly OWNER_TOKEN_SERVICES=("teslatlas-hub.service" "teslatlas-hub-collect.service")
readonly TESLAMATE_PASSWORD_SERVICES=("teslatlas-hub-import@.service")

usage() {
  cat <<'EOF'
Usage:
  install.sh [options]

Secure release mode (requires explicit trust anchors):
  --repo OWNER/REPOSITORY  GitHub release repository
  --release-key FILE       Pinned Minisign public-key file
  [--version TAG]          Release tag, or "latest" (default)

Local test mode:
  --local-artifact FILE    Install a locally built .deb

Common options:
  --dry-run                Check and print actions; change nothing
  --no-start               Install but do not enable or start the service
  --token-file FILE        Import exact token bytes from a protected file
  --prompt-token           Prompt through systemd-ask-password
  --help                   Show this text

No token value is accepted as an argument, environment value, log line, or
plaintext temporary file. Remote mode fails closed without a pinned release
key. --token-file and --prompt-token are mutually exclusive.
EOF
}

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

note() {
  printf '%s\n' "$*"
}

need_root() {
  [[ "${EUID}" -eq 0 ]] || die "run as root, for example: sudo bash install.sh"
}

file_mode() {
  if stat -c '%a' -- "$1" >/dev/null 2>&1; then
    stat -c '%a' -- "$1"
  else
    stat -f '%Lp' "$1"
  fi
}

repo=""
release_key=""
version="latest"
local_artifact=""
token_file=""
prompt_token=0
dry_run=0
no_start=0

while (($#)); do
  case "$1" in
    --repo)
      (($# >= 2)) || die "--repo requires OWNER/REPOSITORY"
      repo="$2"
      shift 2
      ;;
    --release-key)
      (($# >= 2)) || die "--release-key requires a file"
      release_key="$2"
      shift 2
      ;;
    --version)
      (($# >= 2)) || die "--version requires a tag"
      version="$2"
      shift 2
      ;;
    --local-artifact)
      (($# >= 2)) || die "--local-artifact requires a file"
      local_artifact="$2"
      shift 2
      ;;
    --token-file)
      (($# >= 2)) || die "--token-file requires a file"
      token_file="$2"
      shift 2
      ;;
    --prompt-token)
      prompt_token=1
      shift
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --no-start)
      no_start=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ -z "$local_artifact" || (-z "$repo" && -z "$release_key") ]] || \
  die "--local-artifact cannot be combined with --repo or --release-key"
[[ -n "$local_artifact" || -n "$repo" ]] || \
  die "choose --local-artifact or secure release mode with --repo and --release-key"
[[ -z "$repo" || "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || \
  die "--repo must be OWNER/REPOSITORY"
[[ -z "$version" || "$version" =~ ^[A-Za-z0-9._-]+$ ]] || die "unsafe release tag"
[[ -z "$token_file" || "$prompt_token" -eq 0 ]] || \
  die "--token-file and --prompt-token are mutually exclusive"

if [[ -n "$local_artifact" ]]; then
  [[ -f "$local_artifact" ]] || die "local artifact not found"
else
  [[ -f "$release_key" ]] || die "pinned Minisign public key not found"
fi

if [[ -n "$token_file" ]]; then
  [[ -f "$token_file" && ! -L "$token_file" ]] || \
    die "token file must be a regular non-symlink file"
  # Refuse casually-readable credentials. Root can still deliberately provide
  # a protected file owned by another user.
  token_mode="$(file_mode "$token_file")"
  (( (8#$token_mode & 8#077) == 0 )) || die "token file must not be group/world accessible"
fi

if ((dry_run)); then
  note "Dry run. No files, packages, services, or credentials will change."
  if [[ -n "$local_artifact" ]]; then
    command -v dpkg-deb >/dev/null 2>&1 || die "dpkg-deb is required"
    package_name="$(dpkg-deb --field "$local_artifact" Package)"
    package_arch="$(dpkg-deb --field "$local_artifact" Architecture)"
    [[ "$package_name" == "teslatlas-hub" ]] || die "local artifact is not teslatlas-hub"
    note "Would install local artifact for ${package_arch}."
  else
    note "Would verify signed ${version} release from ${repo}, then install it."
  fi
  ((no_start)) || note "Would enable and start teslatlas-hub.service after installation."
  if [[ -n "$token_file" ]]; then
    note "Would encrypt the protected token file with this host's systemd credential key."
  elif ((prompt_token)); then
    note "Would prompt through systemd-ask-password and encrypt directly with this host's systemd credential key."
  fi
  note "Would create or retain a host-encrypted binary cursor signing key for both Hub services."
  exit 0
fi

need_root
[[ -r /etc/os-release ]] || die "unsupported host: /etc/os-release missing"
# shellcheck disable=SC1091
. /etc/os-release
[[ "${ID:-}" == "debian" || "${ID_LIKE:-}" == *debian* ]] || die "supported hosts are Debian-family Linux"

host_arch="$(dpkg --print-architecture)"
case "$host_arch" in
  amd64|arm64) ;;
  *) die "unsupported host architecture: $host_arch" ;;
esac

available_kib="$(df -Pk /var | awk 'NR == 2 { print $4 }')"
[[ "$available_kib" =~ ^[0-9]+$ ]] || die "cannot determine free disk space"
((available_kib >= DEFAULT_MIN_FREE_KIB)) || die "at least 2 GiB free space is required on /var"

command -v flock >/dev/null 2>&1 || die "flock is required"
install -d -m 0755 /run/lock
exec 9>/run/lock/teslatlas-install.lock
flock -n 9 || die "another Teslatlas installation is active"

install_dependencies() {
  command -v apt-get >/dev/null 2>&1 || die "apt-get is required"
  DEBIAN_FRONTEND=noninteractive apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates curl minisign
}

require_credential_tools() {
  command -v systemd-creds >/dev/null 2>&1 || \
    die "systemd-creds is required for Hub credential setup"
  if ((prompt_token)); then
    command -v systemd-ask-password >/dev/null 2>&1 || \
      die "systemd-ask-password is required for --prompt-token"
  fi
}

validate_encrypted_credential() {
  local credential_path="$1"
  local credential_label="$2"
  [[ -f "$credential_path" && ! -L "$credential_path" ]] || \
    die "${credential_label} credential is not a regular file"
  local credential_mode
  credential_mode="$(file_mode "$credential_path")"
  (( (8#$credential_mode & 8#077) == 0 )) || \
    die "${credential_label} credential must not be group/world accessible"
}

write_credential_dropin() {
  local credential_name="$1"
  local credential_path="$2"
  local credential_label="$3"
  shift 3
  validate_encrypted_credential "$credential_path" "$credential_label"

  local service dropin_directory dropin_path dropin_tmp
  for service in "$@"; do
    dropin_directory="/etc/systemd/system/${service}.d"
    dropin_path="${dropin_directory}/10-${credential_name}.conf"
    install -d -m 0755 "$dropin_directory"
    dropin_tmp="$(mktemp "${dropin_directory}/.10-${credential_name}.conf.XXXXXX")"
    if ! printf '[Service]\nLoadCredentialEncrypted=%s:%s\n' \
      "$credential_name" "$credential_path" > "$dropin_tmp"; then
      rm -f -- "$dropin_tmp"
      die "cannot write credential service drop-in"
    fi
    chmod 0644 "$dropin_tmp"
    mv -f -- "$dropin_tmp" "$dropin_path"
  done
}

clear_credential_dropin() {
  local credential_name="$1"
  shift
  local service dropin_directory dropin_path
  for service in "$@"; do
    dropin_directory="/etc/systemd/system/${service}.d"
    dropin_path="${dropin_directory}/10-${credential_name}.conf"
    if [[ -e "$dropin_path" ]]; then
      rm -f -- "$dropin_path"
      rmdir --ignore-fail-on-non-empty "$dropin_directory" 2>/dev/null || true
    fi
  done
}

ensure_cursor_key() {
  install -d -m 0700 "$CREDENTIAL_DIRECTORY"
  if [[ -e "$CURSOR_KEY_PATH" ]]; then
    validate_encrypted_credential "$CURSOR_KEY_PATH" "cursor signing key"
    return
  fi

  [[ -r /dev/urandom ]] || die "/dev/urandom is required for cursor signing key generation"
  command -v head >/dev/null 2>&1 || die "head is required for cursor signing key generation"
  local ciphertext_tmp
  ciphertext_tmp="$(mktemp "${CREDENTIAL_DIRECTORY}/.${CURSOR_KEY_CREDENTIAL}.XXXXXX")"
  if ! head -c 32 /dev/urandom | \
    systemd-creds encrypt --with-key=host --name="$CURSOR_KEY_CREDENTIAL" - "$ciphertext_tmp"; then
    rm -f -- "$ciphertext_tmp"
    die "host-encrypted cursor signing key creation failed"
  fi
  chmod 0600 "$ciphertext_tmp"
  mv -f -- "$ciphertext_tmp" "$CURSOR_KEY_PATH"
}

reconcile_credential_dropins() {
  validate_encrypted_credential "$CURSOR_KEY_PATH" "cursor signing key"
  write_credential_dropin "$CURSOR_KEY_CREDENTIAL" "$CURSOR_KEY_PATH" "cursor signing key" \
    "${CURSOR_KEY_SERVICES[@]}"

  if [[ -e "$OWNER_TOKEN_PATH" ]]; then
    write_credential_dropin "$OWNER_TOKEN_CREDENTIAL" "$OWNER_TOKEN_PATH" "owner token" \
      "${OWNER_TOKEN_SERVICES[@]}"
  else
    clear_credential_dropin "$OWNER_TOKEN_CREDENTIAL" "${OWNER_TOKEN_SERVICES[@]}"
  fi

  if [[ -e "$TESLAMATE_POSTGRES_PASSWORD_PATH" ]]; then
    write_credential_dropin "$TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL" \
      "$TESLAMATE_POSTGRES_PASSWORD_PATH" "TeslaMate PostgreSQL password" \
      "${TESLAMATE_PASSWORD_SERVICES[@]}"
  else
    clear_credential_dropin "$TESLAMATE_POSTGRES_PASSWORD_CREDENTIAL" \
      "${TESLAMATE_PASSWORD_SERVICES[@]}"
  fi
}

encrypt_owner_token_from_file() {
  local ciphertext_tmp
  ciphertext_tmp="$(mktemp "${CREDENTIAL_DIRECTORY}/.${OWNER_TOKEN_CREDENTIAL}.XXXXXX")"
  if ! systemd-creds encrypt --with-key=host --name="$OWNER_TOKEN_CREDENTIAL" \
    "$token_file" "$ciphertext_tmp"; then
    rm -f -- "$ciphertext_tmp"
    die "host-encrypted credential creation failed"
  fi
  chmod 0600 "$ciphertext_tmp"
  mv -f -- "$ciphertext_tmp" "$OWNER_TOKEN_PATH"
}

encrypt_owner_token_from_prompt() {
  local ciphertext_tmp
  ciphertext_tmp="$(mktemp "${CREDENTIAL_DIRECTORY}/.${OWNER_TOKEN_CREDENTIAL}.XXXXXX")"

  if [[ -r /dev/tty ]]; then
    if ! systemd-ask-password --id=teslatlas-owner-token --timeout=0 --echo=no -n \
      "Tesla owner token" </dev/tty | tr -d '\n' | \
      systemd-creds encrypt --with-key=host --name="$OWNER_TOKEN_CREDENTIAL" - "$ciphertext_tmp"; then
      rm -f -- "$ciphertext_tmp"
      die "host-encrypted credential creation failed"
    fi
  elif ! systemd-ask-password --id=teslatlas-owner-token --timeout=0 --echo=no -n \
    "Tesla owner token" | tr -d '\n' | \
    systemd-creds encrypt --with-key=host --name="$OWNER_TOKEN_CREDENTIAL" - "$ciphertext_tmp"; then
    rm -f -- "$ciphertext_tmp"
    die "host-encrypted credential creation failed"
  fi

  chmod 0600 "$ciphertext_tmp"
  mv -f -- "$ciphertext_tmp" "$OWNER_TOKEN_PATH"
}

import_owner_token() {
  install -d -m 0700 "$CREDENTIAL_DIRECTORY"
  if [[ -n "$token_file" ]]; then
    encrypt_owner_token_from_file
  else
    encrypt_owner_token_from_prompt
  fi
}

verify_local_artifact() {
  local artifact="$1"
  local package_name package_arch
  package_name="$(dpkg-deb --field "$artifact" Package)"
  package_arch="$(dpkg-deb --field "$artifact" Architecture)"
  [[ "$package_name" == "teslatlas-hub" ]] || die "artifact package is not teslatlas-hub"
  [[ "$package_arch" == "$host_arch" ]] || die "artifact architecture ${package_arch} does not match ${host_arch}"
}

download_file() {
  local url="$1"
  local destination="$2"
  curl --fail --show-error --silent --location \
    --proto '=https' --proto-redir '=https' --tlsv1.2 \
    --retry 3 --retry-delay 1 --output "$destination" "$url"
}

artifact=""
download_dir=""
cleanup() {
  [[ -z "$download_dir" ]] || rm -rf -- "$download_dir"
}
trap cleanup EXIT HUP INT TERM

require_credential_tools

if [[ -n "$local_artifact" ]]; then
  command -v dpkg-deb >/dev/null 2>&1 || die "dpkg-deb is required"
  verify_local_artifact "$local_artifact"
  artifact="$local_artifact"
else
  install_dependencies
  download_dir="$(mktemp -d /var/tmp/teslatlas-install.XXXXXX)"
  base_url="https://github.com/${repo}/releases"
  if [[ "$version" == "latest" ]]; then
    release_url="${base_url}/latest/download"
  else
    release_url="${base_url}/download/${version}"
  fi

  manifest="$download_dir/manifest"
  signature="$download_dir/manifest.minisig"
  download_file "${release_url}/teslatlas-hub.manifest" "$manifest"
  download_file "${release_url}/teslatlas-hub.manifest.minisig" "$signature"
  minisign -Vm "$manifest" -x "$signature" -p "$release_key" >/dev/null

  manifest_version="$(awk -F= '$1 == "version" { print $2; exit }' "$manifest")"
  manifest_filename="$(awk -F= -v arch="$host_arch" '$1 == "artifact." arch { print $2; exit }' "$manifest")"
  manifest_sha256="$(awk -F= -v arch="$host_arch" '$1 == "sha256." arch { print $2; exit }' "$manifest")"
  [[ -n "$manifest_version" && -n "$manifest_filename" ]] || die "signed manifest lacks a ${host_arch} artifact"
  [[ "$manifest_filename" =~ ^teslatlas-hub_[A-Za-z0-9.+:~_-]+_${host_arch}\.deb$ ]] || die "unsafe artifact name in manifest"
  [[ "$manifest_sha256" =~ ^[0-9a-fA-F]{64}$ ]] || die "invalid artifact checksum in manifest"

  artifact="$download_dir/$manifest_filename"
  download_file "${release_url}/${manifest_filename}" "$artifact"
  printf '%s  %s\n' "$manifest_sha256" "$artifact" | sha256sum --check --status || die "artifact checksum mismatch"
  verify_local_artifact "$artifact"
fi

was_active=0
if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet teslatlas-hub.service; then
  was_active=1
  systemctl stop teslatlas-hub.service
fi

dpkg -i "$artifact"

if [[ -n "$token_file" ]] || ((prompt_token)); then
  import_owner_token
fi
ensure_cursor_key
reconcile_credential_dropins

systemctl daemon-reload

if ((no_start)); then
  note "Installed. Service left stopped by request."
  exit 0
elif ((was_active)); then
  systemctl start teslatlas-hub.service
else
  systemctl enable --now teslatlas-hub.service
fi

if (( ! no_start )); then
  active=0
  for _attempt in $(seq 1 30); do
    if systemctl is-active --quiet teslatlas-hub.service; then
      active=1
      break
    fi
    sleep 1
  done
  ((active)) || die "service did not become active; inspect: journalctl -u teslatlas-hub.service"
fi

config_uses_tls() {
  awk '
    /^[[:space:]]*\[tls\][[:space:]]*(#.*)?$/ { found = 1 }
    END { exit !found }
  ' /etc/teslatlas/config.toml
}

if command -v curl >/dev/null 2>&1 && ! config_uses_tls; then
  configured_bind="$(awk -F'"' '/^[[:space:]]*bind[[:space:]]*=[[:space:]]*"/ { print $2; exit }' /etc/teslatlas/config.toml)"
  [[ -n "$configured_bind" ]] || die "cannot determine configured bind address"
  ready_url="http://${configured_bind}/readyz"
  ready=0
  for _attempt in $(seq 1 30); do
    if curl --fail --silent --show-error --max-time 5 "$ready_url" >/dev/null; then
      ready=1
      break
    fi
    sleep 1
  done
  ((ready)) || die "service is active but readiness verification failed"
else
  # A newly-exec'd unit is "active" before it has finished opening and
  # migrating SQLite. With no HTTP client, wait for that startup work through
  # the diagnostic command instead of racing it once.
  verified=0
  for _attempt in $(seq 1 30); do
    if /usr/bin/teslatlas-hub --config /etc/teslatlas/config.toml doctor >/dev/null; then
      verified=1
      break
    fi
    sleep 1
  done
  ((verified)) || die "service is active but Hub database verification failed"
fi

/usr/bin/teslatlas-hub-verify --config /etc/teslatlas/config.toml

note "Teslatlas Hub is active and ready."
