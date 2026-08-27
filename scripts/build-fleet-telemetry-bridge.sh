#!/bin/sh

set -eu

umask 022
PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
export PATH
GOENV=off
GOWORK=off
GOTOOLCHAIN=local
GOFLAGS=-mod=readonly
GOPROXY=https://proxy.golang.org
GOSUMDB=sum.golang.org
GONOSUMDB=
GOPRIVATE=
export GOENV GOWORK GOTOOLCHAIN GOFLAGS GOPROXY GOSUMDB GONOSUMDB GOPRIVATE

VERSION=v0.9.4
COMMIT=d64c73ab65e7c5fb5fc12b35fe507e2c6054227b
ARCHIVE_URL=https://codeload.github.com/teslamotors/fleet-telemetry/tar.gz/d64c73ab65e7c5fb5fc12b35fe507e2c6054227b
ARCHIVE_SHA256=a30818d9d832cf6dcec7cf0d61b780d4bea52cc7c9f8edb31a111bc0f25cd6b9
PATCH_SHA256=800b8572eb32da0f851316cf6e13349325ca6a3c4470887e8f3dc9bc222d8b59
GO_VERSION=go1.27.0

die() {
    printf '%s\n' "build-fleet-telemetry-bridge: $*" >&2
    exit 1
}

usage() {
    printf '%s\n' \
        'Usage: scripts/build-fleet-telemetry-bridge.sh --target TARGET --output PATH' \
        '' \
        'TARGET: darwin-arm64, darwin-amd64, linux-arm64, or linux-amd64'
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | /usr/bin/awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | /usr/bin/awk '{print $1}'
    else
        die "sha256sum or shasum is required"
    fi
}

target=
output=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            [ "$#" -ge 2 ] || die "--target requires a value"
            target=$2
            shift 2
            ;;
        --output)
            [ "$#" -ge 2 ] || die "--output requires a path"
            output=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            die "unknown argument: $1"
            ;;
    esac
done

[ -n "$target" ] || die "--target is required"
[ -n "$output" ] || die "--output is required"
case "$target" in
    darwin-arm64)
        target_os=darwin
        target_arch=arm64
        file_pattern='Mach-O 64-bit executable arm64'
        ;;
    darwin-amd64)
        target_os=darwin
        target_arch=amd64
        file_pattern='Mach-O 64-bit executable x86_64'
        ;;
    linux-arm64)
        target_os=linux
        target_arch=arm64
        file_pattern='ELF 64-bit LSB executable, ARM aarch64'
        ;;
    linux-amd64)
        target_os=linux
        target_arch=amd64
        file_pattern='ELF 64-bit LSB executable, x86-64'
        ;;
    *)
        die "unsupported target: $target"
        ;;
esac

script_directory=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
repository_root=$(CDPATH='' cd "$script_directory/.." && pwd -P)
lock_file="$repository_root/packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json"
overlay_patch="$repository_root/packaging/fleet-telemetry-bridge/0001-teslatlas-http-dispatcher.patch"
[ -f "$lock_file" ] && [ ! -L "$lock_file" ] || die "lock file is missing or unsafe"
[ -f "$overlay_patch" ] && [ ! -L "$overlay_patch" ] || die "overlay patch is missing or unsafe"

PYTHON=$(command -v python3) || die "python3 is required"
GO=$(command -v go) || die "go is required"
CURL=$(command -v curl) || die "curl is required"
PATCH=$(command -v patch) || die "patch is required"
TAR=$(command -v tar) || die "tar is required"
FILE=$(command -v file) || die "file is required"
[ "$($GO env GOVERSION)" = "$GO_VERSION" ] || die "$GO_VERSION is required exactly"

"$PYTHON" - "$lock_file" "$VERSION" "$COMMIT" "$ARCHIVE_URL" "$ARCHIVE_SHA256" "$PATCH_SHA256" "$GO_VERSION" "$target" <<'PY'
import json
import sys

path, version, commit, archive_url, archive_sha, patch_sha, go_version, target = sys.argv[1:]
with open(path, "rb") as source:
    lock = json.load(source)
expected = {
    "schema": 1,
    "repository": "https://github.com/teslamotors/fleet-telemetry",
    "version": version,
    "commit": commit,
    "archive_url": archive_url,
    "archive_sha256": archive_sha,
    "patch": "0001-teslatlas-http-dispatcher.patch",
    "patch_sha256": patch_sha,
    "endpoint": "http://127.0.0.1:8080/v1/internal/fleet-telemetry",
    "bearer_file_env": "TESLATLAS_FLEET_TELEMETRY_BEARER_FILE",
    "envelope_version": 1,
    "max_envelope_bytes": 262144,
    "default_timeout_ms": 2000,
    "maximum_timeout_ms": 5000,
    "go_version": go_version,
    "cgo_enabled": False,
}
actual = {
    "schema": lock.get("schema"),
    "repository": lock.get("upstream", {}).get("repository"),
    "version": lock.get("upstream", {}).get("version"),
    "commit": lock.get("upstream", {}).get("commit"),
    "archive_url": lock.get("upstream", {}).get("archive_url"),
    "archive_sha256": lock.get("upstream", {}).get("archive_sha256"),
    "patch": lock.get("overlay", {}).get("patch"),
    "patch_sha256": lock.get("overlay", {}).get("patch_sha256"),
    "endpoint": lock.get("bridge", {}).get("endpoint"),
    "bearer_file_env": lock.get("bridge", {}).get("bearer_file_env"),
    "envelope_version": lock.get("bridge", {}).get("envelope_version"),
    "max_envelope_bytes": lock.get("bridge", {}).get("max_envelope_bytes"),
    "default_timeout_ms": lock.get("bridge", {}).get("default_timeout_ms"),
    "maximum_timeout_ms": lock.get("bridge", {}).get("maximum_timeout_ms"),
    "go_version": lock.get("toolchain", {}).get("go_version"),
    "cgo_enabled": lock.get("toolchain", {}).get("cgo_enabled"),
}
if actual != expected:
    raise SystemExit("fleet telemetry bridge lock does not match the build script")
if lock.get("targets") != ["darwin-arm64", "darwin-amd64", "linux-arm64", "linux-amd64"]:
    raise SystemExit("fleet telemetry bridge target lock is invalid")
if target not in lock["targets"]:
    raise SystemExit("requested target is not locked")
PY

[ "$(sha256_file "$overlay_patch")" = "$PATCH_SHA256" ] || die "overlay patch checksum mismatch"

output_parent=$(dirname "$output")
[ -d "$output_parent" ] || die "output directory does not exist: $output_parent"
output_directory=$(CDPATH='' cd "$output_parent" && pwd -P)
output_name=$(basename "$output")
case "$output_name" in
    ''|.|..|/) die "invalid output path" ;;
esac
output="$output_directory/$output_name"
if [ -e "$output" ] || [ -L "$output" ]; then
    [ -f "$output" ] && [ ! -L "$output" ] || die "output is not a regular file"
fi

work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/fleet-telemetry-bridge.XXXXXX")
cleanup() {
    /usr/bin/find "$work" -depth -delete >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM
archive="$work/fleet-telemetry.tar.gz"
source_directory="$work/fleet-telemetry-$COMMIT"
binary="$work/fleet-telemetry-$target"
GOCACHE="$work/go-build-cache"
export GOCACHE
/bin/mkdir -p "$GOCACHE"

"$CURL" --fail --silent --show-error --location --max-redirs 3 \
    --proto '=https' --tlsv1.2 --connect-timeout 15 --retry 2 \
    --output "$archive" "$ARCHIVE_URL" \
    || die "cannot download pinned fleet-telemetry source"
[ "$(sha256_file "$archive")" = "$ARCHIVE_SHA256" ] || die "upstream archive checksum mismatch"

archive_root=$("$TAR" -tzf "$archive" | /usr/bin/awk -F/ 'NF {print $1}' | /usr/bin/sort -u)
[ "$archive_root" = "fleet-telemetry-$COMMIT" ] || die "upstream archive revision mismatch"
"$TAR" -xzf "$archive" -C "$work" || die "cannot extract pinned source"
[ -d "$source_directory" ] && [ ! -L "$source_directory" ] || die "pinned source directory is missing"
(
    cd "$source_directory"
    "$PATCH" -p1 --batch --forward <"$overlay_patch"
) || die "cannot apply Teslatlas overlay"

(
    cd "$source_directory"
    CGO_ENABLED=0 "$GO" test ./datastore/teslatlas
) || die "Teslatlas bridge tests failed"

if [ "$target_arch" = arm64 ]; then
    (
        cd "$source_directory"
        CGO_ENABLED=0 GOOS="$target_os" GOARCH="$target_arch" GOARM64=v8.0 \
            "$GO" build -trimpath -buildvcs=false -ldflags='-s -w' -o "$binary" ./cmd
    ) || die "cannot build $target bridge"
else
    (
        cd "$source_directory"
        CGO_ENABLED=0 GOOS="$target_os" GOARCH="$target_arch" GOAMD64=v1 \
            "$GO" build -trimpath -buildvcs=false -ldflags='-s -w' -o "$binary" ./cmd
    ) || die "cannot build $target bridge"
fi

[ -f "$binary" ] && [ ! -L "$binary" ] || die "bridge build did not produce a regular file"
"$FILE" "$binary" | /usr/bin/grep -F "$file_pattern" >/dev/null \
    || die "bridge binary architecture does not match $target"
/usr/bin/install -m 0755 "$binary" "$output"
printf '%s\n' "$output"
