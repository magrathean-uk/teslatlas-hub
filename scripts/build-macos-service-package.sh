#!/bin/sh

set -eu

umask 022

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
COPYFILE_DISABLE=1
export COPYFILE_DISABLE

usage() {
    cat <<'EOF'
Usage: scripts/build-macos-service-package.sh --binary PATH --version VERSION [--output PATH]

Builds an unsigned local macOS 12+ arm64 installer package. The package never
installs or starts the Hub during its build.
EOF
}

die() {
    printf '%s\n' "build-macos-service-package: $*" >&2
    exit 1
}

binary=
version=
output=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary)
            [ "$#" -ge 2 ] || die "--binary requires a path"
            binary=$2
            shift 2
            ;;
        --version)
            [ "$#" -ge 2 ] || die "--version requires a value"
            version=$2
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

[ -n "$binary" ] || die "--binary is required"
[ -n "$version" ] || die "--version is required"
/usr/bin/printf '%s\n' "$version" | /usr/bin/grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
    || die "version must be semver with an optional prerelease: $version"
package_version=${version%%-*}
[ -f "$binary" ] && [ ! -L "$binary" ] && [ -x "$binary" ] \
    || die "binary must be an executable regular file"

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
TEMPLATE="$ROOT/packaging/macos-service/com.teslatlas.hub.plist.in"
PACKAGE_SCRIPTS="$ROOT/packaging/macos-service/scripts"
[ -f "$TEMPLATE" ] || die "LaunchAgent template is missing"
[ -x "$PACKAGE_SCRIPTS/preinstall" ] || die "preinstall script is not executable"
[ -x "$PACKAGE_SCRIPTS/postinstall" ] || die "postinstall script is not executable"
/usr/bin/plutil -lint "$TEMPLATE" >/dev/null || die "LaunchAgent template is invalid"

if [ -z "$output" ]; then
    output="$(pwd)/teslatlas-hub-${version}-macos12-arm64.pkg"
fi
output_directory=$(CDPATH='' cd "$(dirname "$output")" && pwd)
output="$output_directory/$(basename "$output")"
if [ -e "$output" ] || [ -L "$output" ]; then
    [ -f "$output" ] && [ ! -L "$output" ] \
        || die "output must be a regular file or absent"
fi

staging=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-package.XXXXXX") \
    || die "cannot create staging directory"
cleanup() {
    /bin/rm -rf "$staging"
}
trap cleanup EXIT HUP INT TERM

payload="$staging/payload"
scripts="$staging/scripts"
/bin/mkdir -p "$payload/Library/Application Support/Teslatlas Hub/bin" \
    "$payload/Library/Application Support/Teslatlas Hub/share" "$scripts"
/usr/bin/lipo -verify_arch arm64 "$binary" \
    || die "binary has no arm64 slice"
payload_binary="$payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
architectures=$(/usr/bin/lipo -archs "$binary") \
    || die "cannot inspect binary architectures"
if [ "$architectures" = arm64 ]; then
    /usr/bin/install -m 0755 "$binary" "$payload_binary" \
        || die "cannot copy arm64 binary"
else
    /usr/bin/lipo "$binary" -thin arm64 -output "$payload_binary" \
        || die "cannot extract arm64 binary slice"
fi
/usr/bin/find "$payload/Library/Application Support/Teslatlas Hub/share" -type f -delete
for legal_file in LICENSE NOTICE THIRD_PARTY_NOTICES.md PROVENANCE.md TRADEMARKS.md PRIVACY.md LEGAL.md; do
    if [ -f "$ROOT/$legal_file" ]; then
        /usr/bin/install -m 0644 "$ROOT/$legal_file" \
            "$payload/Library/Application Support/Teslatlas Hub/share/$legal_file"
    fi
done
/usr/bin/xattr -c "$payload_binary" >/dev/null 2>&1 \
    || die "cannot clear Hub binary metadata"
[ "$(/usr/bin/lipo -archs "$payload_binary")" = arm64 ] \
    || die "payload binary is not arm64-only"

minimum_macos=$(
    /usr/bin/otool -l "$payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub" \
        | /usr/bin/awk '
            $1 == "cmd" { command = $2 }
            command == "LC_BUILD_VERSION" && $1 == "minos" { print $2; exit }
            command == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; exit }
        '
)
case "$minimum_macos" in
    ''|*[!0-9.]*) die "cannot read binary macOS deployment target" ;;
esac
minimum_major=${minimum_macos%%.*}
[ "$minimum_major" -le 12 ] \
    || die "binary requires macOS $minimum_macos; macOS 12 compatibility is required"

/usr/bin/install -m 0644 "$TEMPLATE" "$scripts/com.teslatlas.hub.plist.in"
/usr/bin/install -m 0644 "$PACKAGE_SCRIPTS/common.sh" "$scripts/common.sh"
/usr/bin/install -m 0755 "$PACKAGE_SCRIPTS/preinstall" "$scripts/preinstall"
/usr/bin/install -m 0755 "$PACKAGE_SCRIPTS/postinstall" "$scripts/postinstall"
/usr/bin/xattr -c "$scripts/common.sh" "$scripts/preinstall" "$scripts/postinstall" \
    "$scripts/com.teslatlas.hub.plist.in" >/dev/null 2>&1 \
    || die "cannot clear package script metadata"
metadata=$(
    /usr/bin/find "$payload" "$scripts" \( -name '._*' -o -name '.DS_Store' \) -print -quit
)
[ -z "$metadata" ] || die "staging contains AppleDouble metadata"

tmp_package="$staging/TeslatlasHubService.pkg"
/usr/bin/pkgbuild \
    --root "$payload" \
    --scripts "$scripts" \
    --identifier com.teslatlas.hub.service \
    --version "$package_version" \
    --install-location / \
    --ownership recommended \
    "$tmp_package"

expanded="$staging/expanded"
/usr/sbin/pkgutil --expand-full "$tmp_package" "$expanded" \
    || die "cannot inspect generated package payload"
expanded_binary="$expanded/Payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
[ -f "$expanded_binary" ] && [ ! -L "$expanded_binary" ] \
    || die "generated package has the wrong payload path"
[ "$(/usr/bin/lipo -archs "$expanded_binary")" = arm64 ] \
    || die "generated package payload is not arm64-only"
metadata=$(
    /usr/bin/find "$expanded/Payload" "$expanded/Scripts" \
        \( -name '._*' -o -name '.DS_Store' \) -print -quit
)
[ -z "$metadata" ] || die "generated package expands AppleDouble metadata"
/usr/bin/xattr -c "$tmp_package" >/dev/null 2>&1 \
    || die "cannot clear package metadata"
/bin/mv -f "$tmp_package" "$output"

printf '%s\n' "$output"
