#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

readonly PROGRAM_NAME="${0##*/}"

usage() {
  cat <<'EOF'
Usage:
  scripts/write-release-manifest.sh --version VERSION --artifacts DIR --secret-key FILE --out DIR

Expects:
  teslatlas-hub_VERSION_amd64.deb
  teslatlas-hub_VERSION_arm64.deb

Writes a deterministic manifest and a detached Minisign signature. Keep the
secret signing key outside this repository and outside CI logs.
EOF
}

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

version=""
artifacts_dir=""
secret_key=""
out_dir=""

while (($#)); do
  case "$1" in
    --version) version="${2:?--version requires a value}"; shift 2 ;;
    --artifacts) artifacts_dir="${2:?--artifacts requires a directory}"; shift 2 ;;
    --secret-key) secret_key="${2:?--secret-key requires a file}"; shift 2 ;;
    --out) out_dir="${2:?--out requires a directory}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ -n "$version" && -n "$artifacts_dir" && -n "$secret_key" && -n "$out_dir" ]] || {
  usage >&2
  exit 2
}
command -v minisign >/dev/null 2>&1 || die "minisign is required"
[[ -r "$secret_key" ]] || die "secret key not readable"

manifest="$out_dir/teslatlas-hub.manifest"
mkdir -p "$out_dir"
{
  printf 'format=1\n'
  printf 'version=%s\n' "$version"
  for arch in amd64 arm64; do
    artifact="$artifacts_dir/teslatlas-hub_${version}_${arch}.deb"
    [[ -f "$artifact" ]] || die "missing artifact: $artifact"
    printf 'artifact.%s=%s\n' "$arch" "${artifact##*/}"
    printf 'sha256.%s=%s\n' "$arch" "$(sha256sum "$artifact" | awk '{print $1}')"
  done
} > "$manifest"

minisign -Sm "$manifest" -s "$secret_key" -x "$manifest.minisig" -q
printf '%s\n' "$manifest"
