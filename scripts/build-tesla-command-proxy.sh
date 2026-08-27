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

COMMIT=49977a18fd68567501d59e16a6c9e4a8b9348544
VERSION=v0.4.1
GO_VERSION=go1.27.0

die() {
    printf '%s\n' "build-tesla-command-proxy: $*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: scripts/build-tesla-command-proxy.sh --output PATH

Builds Tesla's official tesla-http-proxy. TARGET defaults to darwin-arm64.

Options:
  --target darwin-arm64|linux-amd64|linux-arm64
EOF
}

output=
target=darwin-arm64
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            [ "$#" -ge 2 ] || die "--output requires a path"
            output=$2
            shift 2
            ;;
        --target)
            [ "$#" -ge 2 ] || die "--target requires a value"
            target=$2
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
[ -n "$output" ] || die "--output is required"

case "$target" in
    darwin-arm64)
        target_os=darwin
        target_arch=arm64
        target_cgo=1
        file_pattern='Mach-O 64-bit executable arm64'
        MACOSX_DEPLOYMENT_TARGET=13.0
        COPYFILE_DISABLE=1
        export MACOSX_DEPLOYMENT_TARGET COPYFILE_DISABLE
        ;;
    linux-amd64)
        target_os=linux
        target_arch=amd64
        target_cgo=0
        file_pattern='ELF 64-bit LSB.*x86-64'
        ;;
    linux-arm64)
        target_os=linux
        target_arch=arm64
        target_cgo=0
        file_pattern='ELF 64-bit LSB.*ARM aarch64'
        ;;
    *) die "unsupported target: $target" ;;
esac

GO=$(command -v go) || die "go is required"
[ -x "$GO" ] || die "go is not executable"
go_version=$($GO env GOVERSION)
[ "$go_version" = "$GO_VERSION" ] \
    || die "$GO_VERSION is required exactly: $go_version"

output_directory=$(CDPATH='' cd "$(dirname "$output")" && pwd)
output="$output_directory/$(basename "$output")"
if [ -e "$output" ] || [ -L "$output" ]; then
    [ -f "$output" ] && [ ! -L "$output" ] || die "output is not a regular file"
fi

work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/tesla-command-proxy.XXXXXX")
cleanup() {
    /usr/bin/find "$work" -depth -delete >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM
GOCACHE="$work/build-cache"
export GOCACHE
/bin/mkdir -p "$GOCACHE"

MODULE=github.com/teslamotors/vehicle-command
MODULE_SUM=h1:J4ne/TNGwgodJLYJDLm/hjoygXyQ/bpqO/EiCaeoobM=
MODULE_GOSUM=h1:liN6VG6MCc7m02wFaBm2sQT6MYGm/dJua6bG00QSpnA=

module_json=$($GO mod download -json "$MODULE@$VERSION") \
    || die "cannot download Tesla vehicle-command module"
module_path=$(printf '%s\n' "$module_json" | /usr/bin/awk -F'"' '$2 == "Path" { print $4; exit }')
module_version=$(printf '%s\n' "$module_json" | /usr/bin/awk -F'"' '$2 == "Version" { print $4; exit }')
module_dir=$(printf '%s\n' "$module_json" | /usr/bin/awk -F'"' '$2 == "Dir" { print $4; exit }')
module_sum=$(printf '%s\n' "$module_json" | /usr/bin/awk -F'"' '$2 == "Sum" { print $4; exit }')
module_gosum=$(printf '%s\n' "$module_json" | /usr/bin/awk -F'"' '$2 == "GoModSum" { print $4; exit }')
origin_hash=$(printf '%s\n' "$module_json" | /usr/bin/awk -F'"' '$2 == "Hash" { print $4; exit }')
[ "$module_path" = "$MODULE" ] || die "unexpected Tesla module: $module_path"
[ "$module_version" = "$VERSION" ] || die "unexpected Tesla module version: $module_version"
[ "$module_sum" = "$MODULE_SUM" ] || die "unexpected Tesla module sum: $module_sum"
[ "$module_gosum" = "$MODULE_GOSUM" ] || die "unexpected Tesla go.mod sum: $module_gosum"
[ "$origin_hash" = "$COMMIT" ] || die "unexpected Tesla module revision: $origin_hash"
[ -d "$module_dir" ] || die "Tesla module cache directory is missing"

# Reconstruct the main module in a private cache from the checksum-verified
# download proxy. Never compile the mutable extracted source tree in the shared
# Go module cache.
host_module_cache=$($GO env GOMODCACHE)
case "$host_module_cache" in
    /*) ;;
    *) die "Go module cache path is not absolute" ;;
esac
host_file_proxy="$host_module_cache/cache/download"
[ -d "$host_file_proxy" ] || die "Go module download cache is missing"
GOMODCACHE="$work/module-cache"
GOPROXY="file://$host_file_proxy"
GOSUMDB=off
export GOMODCACHE GOPROXY GOSUMDB
private_module_json=$($GO mod download -json "$MODULE@$VERSION") \
    || die "cannot reconstruct Tesla source in the private module cache"
private_module_dir=$(printf '%s\n' "$private_module_json" | /usr/bin/awk -F'"' '$2 == "Dir" { print $4; exit }')
private_module_sum=$(printf '%s\n' "$private_module_json" | /usr/bin/awk -F'"' '$2 == "Sum" { print $4; exit }')
private_module_gosum=$(printf '%s\n' "$private_module_json" | /usr/bin/awk -F'"' '$2 == "GoModSum" { print $4; exit }')
[ "$private_module_sum" = "$MODULE_SUM" ] || die "private Tesla module sum changed"
[ "$private_module_gosum" = "$MODULE_GOSUM" ] || die "private Tesla go.mod sum changed"
[ -d "$private_module_dir" ] || die "private Tesla source directory is missing"

# Upstream still declares an older Go language version. Build from a private
# copy and add only a build-time GODEBUG policy. The cached/tagged source stays
# untouched while the executable uses Go 1.27's secure runtime defaults.
/bin/cp -R "$private_module_dir" "$work/source"
/bin/chmod -R u+w "$work/source"
# Populate the host download proxy from the verified main source, then return
# to the private module cache for the actual build. Only the locked runtime
# graph is requested; the shared extracted module directory is never read.
(
    cd "$work/source"
    GOMODCACHE="$host_module_cache" GOCACHE="$work/discovery-cache" \
        GOPROXY=https://proxy.golang.org GOSUMDB=sum.golang.org \
        GOWORK=off $GO list -mod=readonly -deps ./cmd/tesla-http-proxy >/dev/null
) || die "cannot cache the locked Tesla proxy dependencies"
(
    cd "$work/source"
    GOWORK=off $GO mod edit -godebug=default=go1.27
)

if [ "$target" = darwin-arm64 ]; then
    (
        cd "$work/source"
        GOWORK=off CGO_ENABLED=1 GOOS=darwin GOARCH=arm64 \
            $GO build -trimpath -buildvcs=false -ldflags='-s -w' \
            -o "$work/tesla-http-proxy" ./cmd/tesla-http-proxy \
            || die "cannot build Tesla command proxy"
    )
else
    (
        cd "$work/source"
        GOWORK=off CGO_ENABLED="$target_cgo" GOOS="$target_os" GOARCH="$target_arch" \
            $GO build -trimpath -buildvcs=false -ldflags='-s -w' \
            -o "$work/tesla-http-proxy" ./cmd/tesla-http-proxy \
            || die "cannot build Tesla command proxy for $target"
    )
fi

[ -f "$work/tesla-http-proxy" ] && [ ! -L "$work/tesla-http-proxy" ] \
    || die "proxy build did not produce a regular file"
/usr/bin/file "$work/tesla-http-proxy" | /usr/bin/grep -Eq "$file_pattern" \
    || die "proxy architecture does not match $target"
if [ "$target" = darwin-arm64 ]; then
    /usr/bin/otool -l "$work/tesla-http-proxy" | /usr/bin/awk '
        $1 == "cmd" { command = $2 }
        command == "LC_BUILD_VERSION" && $1 == "minos" { print $2; exit }
        command == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; exit }
    ' | /usr/bin/grep -Eq '^([0-9]|1[0-2])\.|^13(\.|$)' \
        || die "proxy requires newer than macOS 13"
fi
/usr/bin/install -m 0755 "$work/tesla-http-proxy" "$output"

printf '%s\n' "$output"
