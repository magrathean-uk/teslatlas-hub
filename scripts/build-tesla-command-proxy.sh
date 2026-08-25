#!/bin/sh

set -eu

umask 022
PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin
export PATH
MACOSX_DEPLOYMENT_TARGET=12.0
export MACOSX_DEPLOYMENT_TARGET
COPYFILE_DISABLE=1
export COPYFILE_DISABLE

COMMIT=49977a18fd68567501d59e16a6c9e4a8b9348544
VERSION=v0.4.1

die() {
    printf '%s\n' "build-tesla-command-proxy: $*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: scripts/build-tesla-command-proxy.sh --output PATH

Builds Tesla's official tesla-http-proxy for macOS 12+ arm64.
EOF
}

output=
while [ "$#" -gt 0 ]; do
    case "$1" in
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
[ -n "$output" ] || die "--output is required"

GO=$(command -v go) || die "go is required"
[ -x "$GO" ] || die "go is not executable"
go_version=$($GO env GOVERSION)
case "$go_version" in
    go1.2[3-9].*|go1.[3-9][0-9].*) ;;
    *) die "Go 1.23 or newer is required: $go_version" ;;
esac

output_directory=$(CDPATH='' cd "$(dirname "$output")" && pwd)
output="$output_directory/$(basename "$output")"
if [ -e "$output" ] || [ -L "$output" ]; then
    [ -f "$output" ] && [ ! -L "$output" ] || die "output is not a regular file"
fi

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

work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/tesla-command-proxy.XXXXXX")
cleanup() {
    /usr/bin/find "$work" -depth -delete >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

(
    cd "$module_dir"
    CGO_ENABLED=1 GOOS=darwin GOARCH=arm64 \
        $GO build -trimpath -buildvcs=false -ldflags='-s -w' \
        -o "$work/tesla-http-proxy" ./cmd/tesla-http-proxy \
        || die "cannot build Tesla command proxy"
)

[ -f "$work/tesla-http-proxy" ] && [ ! -L "$work/tesla-http-proxy" ] \
    || die "proxy build did not produce a regular file"
/usr/bin/file "$work/tesla-http-proxy" | /usr/bin/grep -Eq 'Mach-O 64-bit executable arm64$' \
    || die "proxy is not an arm64 Mach-O executable"
/usr/bin/otool -l "$work/tesla-http-proxy" | /usr/bin/awk '
    $1 == "cmd" { command = $2 }
    command == "LC_BUILD_VERSION" && $1 == "minos" { print $2; exit }
    command == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; exit }
' | /usr/bin/grep -Eq '^([0-9]|1[0-1])\.|^12(\.|$)' \
    || die "proxy requires newer than macOS 12"
/usr/bin/install -m 0755 "$work/tesla-http-proxy" "$output"

printf '%s\n' "$output"
