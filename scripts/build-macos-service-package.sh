#!/bin/sh

set -eu

umask 022

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
COPYFILE_DISABLE=1
export COPYFILE_DISABLE

usage() {
    cat <<'EOF'
Usage: scripts/build-macos-service-package.sh --binary PATH --proxy-binary PATH --fleet-telemetry-binary PATH --version VERSION [--output PATH]

Builds an unsigned local macOS 13+ arm64 installer package. The package never
installs or starts the Hub during its build.
EOF
}

die() {
    printf '%s\n' "build-macos-service-package: $*" >&2
    exit 1
}

# BEGIN TESTABLE MACH-O HELPER
is_executable_macho() {
    /usr/bin/otool -hv "$1" 2>/dev/null | /usr/bin/awk '
        $1 ~ /^MH_MAGIC(_64)?$/ && $5 == "EXECUTE" { executable = 1 }
        END { exit(executable ? 0 : 1) }
    '
}
# END TESTABLE MACH-O HELPER

minimum_macos() {
    /usr/bin/otool -l "$1" | /usr/bin/awk '
        $1 == "cmd" { command = $2 }
        command == "LC_BUILD_VERSION" && $1 == "minos" { print $2; exit }
        command == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; exit }
    '
}

binary=
proxy_binary=
fleet_telemetry_binary=
version=
output=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary)
            [ "$#" -ge 2 ] || die "--binary requires a path"
            binary=$2
            shift 2
            ;;
        --proxy-binary)
            [ "$#" -ge 2 ] || die "--proxy-binary requires a path"
            proxy_binary=$2
            shift 2
            ;;
        --fleet-telemetry-binary)
            [ "$#" -ge 2 ] || die "--fleet-telemetry-binary requires a path"
            fleet_telemetry_binary=$2
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
[ -n "$proxy_binary" ] || die "--proxy-binary is required"
[ -n "$fleet_telemetry_binary" ] || die "--fleet-telemetry-binary is required"
[ -n "$version" ] || die "--version is required"
/usr/bin/printf '%s\n' "$version" | /usr/bin/grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
    || die "version must be semver with an optional prerelease: $version"
package_base_version=${version%%-*}
case "$version" in
    *-alpha.[0-9]*) package_version="${package_base_version}a${version##*-alpha.}" ;;
    *-beta.[0-9]*) package_version="${package_base_version}b${version##*-beta.}" ;;
    *-rc.[0-9]*) package_version="${package_base_version}fc${version##*-rc.}" ;;
    *-*) die "unsupported prerelease for macOS package version: $version" ;;
    *) package_version=$package_base_version ;;
esac
[ -f "$binary" ] && [ ! -L "$binary" ] && [ -x "$binary" ] \
    || die "binary must be an executable regular file"
[ -f "$proxy_binary" ] && [ ! -L "$proxy_binary" ] && [ -x "$proxy_binary" ] \
    || die "proxy binary must be an executable regular file"
[ -f "$fleet_telemetry_binary" ] && [ ! -L "$fleet_telemetry_binary" ] && [ -x "$fleet_telemetry_binary" ] \
    || die "Fleet Telemetry binary must be an executable regular file"

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
TEMPLATE="$ROOT/packaging/macos-service/com.teslatlas.hub.plist.in"
PACKAGE_SCRIPTS="$ROOT/packaging/macos-service/scripts"
[ -f "$TEMPLATE" ] || die "LaunchAgent template is missing"
[ -x "$PACKAGE_SCRIPTS/preinstall" ] || die "preinstall script is not executable"
[ -x "$PACKAGE_SCRIPTS/postinstall" ] || die "postinstall script is not executable"
[ -x "$PACKAGE_SCRIPTS/uninstall-macos-service.sh" ] || die "uninstall script is not executable"
[ -x "$PACKAGE_SCRIPTS/run-hub-service.sh" ] || die "service supervisor is not executable"
[ -f "$ROOT/packaging/macos-service/fleet-telemetry.json.example" ] \
    || die "Fleet Telemetry config example is missing"
"$ROOT/scripts/test-macos-packaging.sh" >/dev/null \
    || die "macOS packaging source checks failed"
/usr/bin/plutil -lint "$TEMPLATE" >/dev/null || die "LaunchAgent template is invalid"

if [ -z "$output" ]; then
    output="$(pwd)/teslatlas-hub-${version}-macos13-arm64.pkg"
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
    "$payload/Library/Application Support/Teslatlas Hub/libexec" \
    "$payload/Library/Application Support/Teslatlas Hub/share" "$scripts"
/usr/bin/lipo -verify_arch arm64 "$binary" \
    || die "binary has no arm64 slice"
/usr/bin/lipo -verify_arch arm64 "$proxy_binary" \
    || die "proxy binary has no arm64 slice"
/usr/bin/lipo -verify_arch arm64 "$fleet_telemetry_binary" \
    || die "Fleet Telemetry binary has no arm64 slice"
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
/usr/bin/lipo -archs "$proxy_binary" | /usr/bin/grep -qx arm64 \
    || die "proxy binary must be arm64-only"
payload_proxy_binary="$payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
/usr/bin/install -m 0755 "$proxy_binary" "$payload_proxy_binary" \
    || die "cannot copy Tesla command proxy"
/usr/bin/lipo -archs "$fleet_telemetry_binary" | /usr/bin/grep -qx arm64 \
    || die "Fleet Telemetry binary must be arm64-only"
payload_fleet_telemetry_binary="$payload/Library/Application Support/Teslatlas Hub/bin/fleet-telemetry"
/usr/bin/install -m 0755 "$fleet_telemetry_binary" "$payload_fleet_telemetry_binary" \
    || die "cannot copy Fleet Telemetry receiver"
/usr/bin/find "$payload/Library/Application Support/Teslatlas Hub/share" -type f -delete
for legal_file in LICENSE NOTICE THIRD_PARTY_NOTICES.md PROVENANCE.md TRADEMARKS.md PRIVACY.md LEGAL.md; do
    if [ -f "$ROOT/$legal_file" ]; then
        /usr/bin/install -m 0644 "$ROOT/$legal_file" \
            "$payload/Library/Application Support/Teslatlas Hub/share/$legal_file"
    fi
done
/usr/bin/install -m 0644 "$PACKAGE_SCRIPTS/common.sh" \
    "$payload/Library/Application Support/Teslatlas Hub/libexec/common.sh"
/usr/bin/install -m 0755 "$PACKAGE_SCRIPTS/uninstall-macos-service.sh" \
    "$payload/Library/Application Support/Teslatlas Hub/libexec/uninstall-macos-service.sh"
/usr/bin/install -m 0755 "$PACKAGE_SCRIPTS/run-hub-service.sh" \
    "$payload/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh"
/usr/bin/install -m 0644 "$ROOT/packaging/macos-service/fleet-telemetry.json.example" \
    "$payload/Library/Application Support/Teslatlas Hub/share/fleet-telemetry.json.example"
/usr/bin/xattr -c "$payload_binary" "$payload_proxy_binary" "$payload_fleet_telemetry_binary" >/dev/null 2>&1 \
    || die "cannot clear Hub binary metadata"
[ "$(/usr/bin/lipo -archs "$payload_binary")" = arm64 ] \
    || die "payload binary is not arm64-only"
is_executable_macho "$payload_binary" \
    || die "payload binary is not a Mach-O executable"
is_executable_macho "$payload_proxy_binary" \
    || die "payload proxy binary is not a Mach-O executable"
is_executable_macho "$payload_fleet_telemetry_binary" \
    || die "payload Fleet Telemetry receiver is not a Mach-O executable"

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
[ "$minimum_major" -le 13 ] \
    || die "binary requires macOS $minimum_macos; macOS 13 compatibility is required"
proxy_minimum_macos=$(minimum_macos "$payload_proxy_binary")
case "$proxy_minimum_macos" in
    ''|*[!0-9.]*) die "cannot read proxy macOS deployment target" ;;
esac
proxy_minimum_major=${proxy_minimum_macos%%.*}
[ "$proxy_minimum_major" -le 13 ] \
    || die "proxy requires macOS $proxy_minimum_macos; macOS 13 compatibility is required"
fleet_telemetry_minimum_macos=$(minimum_macos "$payload_fleet_telemetry_binary")
case "$fleet_telemetry_minimum_macos" in
    ''|*[!0-9.]*) die "cannot read Fleet Telemetry macOS deployment target" ;;
esac
fleet_telemetry_minimum_major=${fleet_telemetry_minimum_macos%%.*}
[ "$fleet_telemetry_minimum_major" -le 13 ] \
    || die "Fleet Telemetry receiver requires macOS $fleet_telemetry_minimum_macos; macOS 13 compatibility is required"

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
pkgbuild_log="$staging/pkgbuild.log"
if ! /usr/bin/pkgbuild --quiet \
    --root "$payload" \
    --scripts "$scripts" \
    --identifier com.teslatlas.hub.service \
    --version "$package_version" \
    --install-location / \
    --ownership recommended \
    "$tmp_package" >"$pkgbuild_log" 2>&1; then
    /bin/cat "$pkgbuild_log" >&2
    die "pkgbuild failed"
fi
# macOS 27's pkgbuild emits four harmless ownership-probe diagnostics when it
# applies the required root:wheel recommendations from an unprivileged build.
# Never switch to `preserve`: that would archive the developer's UID. Suppress
# only this exact known diagnostic and fail closed for every other message.
permission_warning_count=$(
    /usr/bin/awk '$0 == "write: Permission denied" { count += 1 } END { print count + 0 }' \
        "$pkgbuild_log"
)
case "$permission_warning_count" in
    0|4) ;;
    *)
        /bin/cat "$pkgbuild_log" >&2
        die "pkgbuild emitted an unexpected ownership diagnostic count"
        ;;
esac
unexpected_pkgbuild_output=$(
    /usr/bin/sed '/^write: Permission denied$/d; /^[[:space:]]*$/d' "$pkgbuild_log"
)
[ -z "$unexpected_pkgbuild_output" ] || {
    /usr/bin/printf '%s\n' "$unexpected_pkgbuild_output" >&2
    die "pkgbuild emitted unexpected diagnostics"
}

expanded="$staging/expanded"
/usr/sbin/pkgutil --expand-full "$tmp_package" "$expanded" \
    || die "cannot inspect generated package payload"
expanded_binary="$expanded/Payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
[ -f "$expanded_binary" ] && [ ! -L "$expanded_binary" ] \
    || die "generated package has the wrong payload path"
[ "$(/usr/bin/lipo -archs "$expanded_binary")" = arm64 ] \
    || die "generated package payload is not arm64-only"
is_executable_macho "$expanded_binary" \
    || die "generated package payload is not a Mach-O executable"
expanded_proxy_binary="$expanded/Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
[ -f "$expanded_proxy_binary" ] && [ ! -L "$expanded_proxy_binary" ] \
    || die "generated package has no Tesla command proxy"
[ "$(/usr/bin/lipo -archs "$expanded_proxy_binary")" = arm64 ] \
    || die "generated package proxy is not arm64-only"
is_executable_macho "$expanded_proxy_binary" \
    || die "generated package proxy is not a Mach-O executable"
expanded_fleet_telemetry_binary="$expanded/Payload/Library/Application Support/Teslatlas Hub/bin/fleet-telemetry"
[ -f "$expanded_fleet_telemetry_binary" ] && [ ! -L "$expanded_fleet_telemetry_binary" ] \
    || die "generated package has no Fleet Telemetry receiver"
[ "$(/usr/bin/lipo -archs "$expanded_fleet_telemetry_binary")" = arm64 ] \
    || die "generated package Fleet Telemetry receiver is not arm64-only"
is_executable_macho "$expanded_fleet_telemetry_binary" \
    || die "generated package Fleet Telemetry receiver is not a Mach-O executable"
expanded_supervisor="$expanded/Payload/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh"
[ -f "$expanded_supervisor" ] && [ ! -L "$expanded_supervisor" ] && [ -x "$expanded_supervisor" ] \
    || die "generated package is missing the Fleet receiver supervisor"
expanded_fleet_telemetry_example="$expanded/Payload/Library/Application Support/Teslatlas Hub/share/fleet-telemetry.json.example"
[ -f "$expanded_fleet_telemetry_example" ] && [ ! -L "$expanded_fleet_telemetry_example" ] \
    || die "generated package is missing the Fleet receiver config example"
expanded_uninstaller="$expanded/Payload/Library/Application Support/Teslatlas Hub/libexec/uninstall-macos-service.sh"
[ -f "$expanded_uninstaller" ] && [ ! -L "$expanded_uninstaller" ] && [ -x "$expanded_uninstaller" ] \
    || die "generated package is missing the privileged uninstaller"
metadata=$(
    /usr/bin/find "$expanded/Payload" "$expanded/Scripts" \
        \( -name '._*' -o -name '.DS_Store' \) -print -quit
)
[ -z "$metadata" ] || die "generated package expands AppleDouble metadata"
/usr/bin/xattr -c "$tmp_package" >/dev/null 2>&1 \
    || die "cannot clear package metadata"
/bin/mv -f "$tmp_package" "$output"

printf '%s\n' "$output"
