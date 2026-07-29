#!/usr/bin/env bash
# Build one reviewed Teslatlas Hub commit natively, package it, and hand the
# artifact to the credential-safe Debian installer. No container is involved.
set -euo pipefail
IFS=$'\n\t'
set +x
umask 077

readonly PROGRAM_NAME="${0##*/}"
readonly BUILD_ROOT="/opt/teslatlas-build"
readonly CARGO_HOME="${BUILD_ROOT}/cargo"
readonly RUSTUP_HOME="${BUILD_ROOT}/rustup"
export CARGO_HOME RUSTUP_HOME
export PATH="${CARGO_HOME}/bin:${PATH}"

usage() {
  cat <<'EOF'
Usage:
  bootstrap-from-git.sh --repo HTTPS_URL --ref REF --commit SHA [options]

Builds exactly one Git commit on Debian amd64 or arm64, creates a local Debian
package, then calls install.sh. This is native: it never uses Docker.

Required:
  --repo HTTPS_URL        Git repository URL without credentials or query
  --ref REF               Branch or tag containing the reviewed commit
  --commit SHA            Exact 40- or 64-hex Git object ID to build

Credential options (passed directly to install.sh):
  --token-file FILE       Import protected token bytes
  --prompt-token          Prompt with systemd-ask-password

Other options:
  --dry-run               Validate arguments and print actions; change nothing
  --no-start              Install without starting the service
  --keep-source           Retain the checked-out build directory on success
  --help                  Show this text

Example:
  curl -fsSLO https://example.invalid/bootstrap-from-git.sh
  sudo bash bootstrap-from-git.sh --repo https://github.com/OWNER/REPO.git \
    --ref v0.1.0 --commit FULL_COMMIT_SHA --prompt-token

The source is fetched over HTTPS, then the fetched HEAD must equal --commit.
Use a commit from a separately reviewed signed release. Fleet login is not a
bootstrap feature yet; token-first operation is deliberate.
EOF
}

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

note() {
  printf '%s\n' "$*"
}

repo=""
ref=""
commit=""
token_file=""
prompt_token=0
no_start=0
keep_source=0
dry_run=0

while (($#)); do
  case "$1" in
    --repo)
      (($# >= 2)) || die "--repo requires an HTTPS URL"
      repo="$2"
      shift 2
      ;;
    --ref)
      (($# >= 2)) || die "--ref requires a branch or tag"
      ref="$2"
      shift 2
      ;;
    --commit)
      (($# >= 2)) || die "--commit requires a full object ID"
      commit="$2"
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
    --keep-source)
      keep_source=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ "$repo" =~ ^https://[^/@?#[:space:]]+(/[^?#[:space:]]*)?\.git$ ]] || \
  die "--repo must be a credential-free HTTPS Git URL ending in .git"
[[ "$ref" =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ && "$ref" != *..* && "$ref" != */. && "$ref" != ./* ]] || \
  die "--ref must be a safe branch or tag name"
[[ "$commit" =~ ^[0-9a-fA-F]{40}([0-9a-fA-F]{24})?$ ]] || \
  die "--commit must be a full SHA-1 or SHA-256 Git object ID"
[[ -z "$token_file" || "$prompt_token" -eq 0 ]] || \
  die "--token-file and --prompt-token are mutually exclusive"
[[ -z "$token_file" || ( -f "$token_file" && ! -L "$token_file" ) ]] || \
  die "--token-file must be a regular non-symlink file"

if ((dry_run)); then
  note "dry-run: would clone ${repo} at ${ref}"
  note "dry-run: would require exact commit ${commit}"
  note "dry-run: would build release binary and package with packaging/build-deb.sh"
  if [[ -n "$token_file" ]]; then
    note "dry-run: would pass --token-file (path redacted) to install.sh"
  elif ((prompt_token)); then
    note "dry-run: would pass --prompt-token to install.sh"
  else
    note "dry-run: would install without an owner token"
  fi
  if ((no_start)); then
    note "dry-run: would pass --no-start to install.sh"
  fi
  note "dry-run: no packages, network package installs, credentials, or services changed"
  exit 0
fi

[[ "${EUID}" -eq 0 ]] || die "run as root, for example: sudo bash $PROGRAM_NAME"
[[ -r /etc/os-release ]] || die "unsupported host: /etc/os-release missing"
# shellcheck disable=SC1091
. /etc/os-release
[[ "${ID:-}" == "debian" || "${ID_LIKE:-}" == *debian* ]] || \
  die "supported hosts are Debian-family Linux"

host_arch="$(dpkg --print-architecture)"
case "$host_arch" in
  amd64|arm64) ;;
  *) die "unsupported host architecture: $host_arch" ;;
esac

command -v apt-get >/dev/null 2>&1 || die "apt-get is required"
DEBIAN_FRONTEND=noninteractive apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
  binutils build-essential ca-certificates clang curl dpkg-dev git libclang-dev \
  libssl-dev pkg-config xz-utils

install -d -m 0755 "$BUILD_ROOT"
if [[ ! -x "${CARGO_HOME}/bin/rustup" ]]; then
  rustup_installer="$(mktemp /var/tmp/teslatlas-rustup.XXXXXX)"
  trap 'rm -f -- "$rustup_installer"' EXIT HUP INT TERM
  curl --fail --show-error --silent --location \
    --proto '=https' --proto-redir '=https' --tlsv1.2 \
    --output "$rustup_installer" https://sh.rustup.rs
  RUSTUP_HOME="$RUSTUP_HOME" CARGO_HOME="$CARGO_HOME" \
    sh "$rustup_installer" -y --profile minimal --default-toolchain none
  rm -f -- "$rustup_installer"
  trap - EXIT HUP INT TERM
fi
"${CARGO_HOME}/bin/rustup" toolchain install stable --profile minimal

source_dir="$(mktemp -d /var/tmp/teslatlas-hub-bootstrap.XXXXXX)"
cleanup_source() {
  if ((keep_source)); then
    note "Checked-out source retained at ${source_dir}"
  else
    [[ "$source_dir" == /var/tmp/teslatlas-hub-bootstrap.* ]] || exit 1
    rm -rf -- "$source_dir"
  fi
}
trap cleanup_source EXIT HUP INT TERM

git clone --depth 1 --branch "$ref" "$repo" "$source_dir"
actual_commit="$(git -C "$source_dir" rev-parse HEAD)"
[[ "${actual_commit,,}" == "${commit,,}" ]] || \
  die "fetched ${actual_commit}, not the requested reviewed commit"
git -C "$source_dir" diff --quiet
[[ -z "$(git -C "$source_dir" status --porcelain --untracked-files=no)" ]] || \
  die "checked-out source contains tracked changes"

cargo +stable build --manifest-path "$source_dir/Cargo.toml" --release --locked
version="$(sed -nE 's/^version = "([^"]+)"$/\1/p' "$source_dir/Cargo.toml" | head -n 1)"
[[ -n "$version" ]] || die "cannot read package version from Cargo.toml"
artifact="$source_dir/dist/teslatlas-hub_${version}_${host_arch}.deb"
"$source_dir/packaging/build-deb.sh" --version "$version"
[[ -f "$artifact" ]] || die "native package was not produced"

install_args=(--local-artifact "$artifact")
[[ -z "$token_file" ]] || install_args+=(--token-file "$token_file")
((prompt_token)) && install_args+=(--prompt-token)
((no_start)) && install_args+=(--no-start)
"$source_dir/scripts/install.sh" "${install_args[@]}"
note "Installed native Teslatlas Hub from ${actual_commit}."
