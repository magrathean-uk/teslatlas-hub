#!/bin/sh
set -eu

usage() {
    echo "usage: $0 --binary PATH --receiver-binary PATH --version VERSION --output PATH" >&2
    exit 64
}

binary=
receiver_binary=
version=
output=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) [ "$#" -ge 2 ] || usage; binary=$2; shift 2 ;;
        --receiver-binary) [ "$#" -ge 2 ] || usage; receiver_binary=$2; shift 2 ;;
        --version) [ "$#" -ge 2 ] || usage; version=$2; shift 2 ;;
        --output) [ "$#" -ge 2 ] || usage; output=$2; shift 2 ;;
        *) usage ;;
    esac
done
[ -n "$binary" ] && [ -n "$receiver_binary" ] && [ -n "$version" ] && [ -n "$output" ] || usage

regular_executable() {
    input=$1
    label=$2
    [ -f "$input" ] && [ ! -L "$input" ] && [ -x "$input" ] || {
        echo "$label must be a non-symlink executable" >&2
        exit 65
    }
    input_directory=$(CDPATH='' cd -- "$(dirname -- "$input")" && pwd)
    printf '%s/%s\n' "$input_directory" "$(basename -- "$input")"
}
binary=$(regular_executable "$binary" 'Edge binary')
receiver_binary=$(regular_executable "$receiver_binary" 'receiver binary')
command -v pkgbuild >/dev/null 2>&1 || { echo 'pkgbuild is required on macOS' >&2; exit 69; }
command -v productbuild >/dev/null 2>&1 || { echo 'productbuild is required on macOS' >&2; exit 69; }
command -v file >/dev/null 2>&1 || { echo 'file is required' >&2; exit 69; }
for candidate in "$binary" "$receiver_binary"; do
    file_output=$(file -b "$candidate")
    case "$file_output" in
        *'Mach-O'*arm64*) ;;
        *) echo "not a Darwin arm64 binary: $candidate" >&2; exit 65 ;;
    esac
done
version_output=$("$binary" --version 2>&1) || { echo 'Edge binary version query failed' >&2; exit 65; }
[ "$version_output" = "teslatlas-edge $version" ] || { echo 'Edge binary version mismatch' >&2; exit 65; }
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' || { echo 'invalid version' >&2; exit 65; }
[ ! -e "$output" ] && [ ! -L "$output" ] || { echo 'output already exists' >&2; exit 65; }

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-edge-pkg.XXXXXX")
trap 'find -x "$stage" -depth -delete' EXIT HUP INT TERM
core_root="$stage/core-root"
receiver_root="$stage/receiver-root"
install -d \
    "$core_root/usr/local/libexec/teslatlas-edge" \
    "$core_root/Library/Application Support/Teslatlas Edge/launchagents" \
    "$receiver_root/usr/local/libexec/teslatlas-edge" \
    "$receiver_root/Library/Application Support/Teslatlas Edge/launchagents"
install -m 0755 "$binary" "$core_root/usr/local/libexec/teslatlas-edge/teslatlas-edge"
install -m 0755 "$root/scripts/run-with-spool-format-guard.sh" \
    "$core_root/usr/local/libexec/teslatlas-edge/run-with-spool-format-guard.sh"
install -m 0755 "$root/packaging/macos/scripts/teslatlas-edge-service.sh" \
    "$core_root/usr/local/libexec/teslatlas-edge/teslatlas-edge-service.sh"
install -m 0755 "$root/packaging/macos/scripts/uninstall-teslatlas-edge.sh" \
    "$core_root/usr/local/libexec/teslatlas-edge/uninstall-teslatlas-edge.sh"
install -m 0644 "$root/packaging/macos/uk.co.magrathean.teslatlas-edge.plist" \
    "$core_root/Library/Application Support/Teslatlas Edge/launchagents/uk.co.magrathean.teslatlas-edge.plist"
install -m 0755 "$receiver_binary" \
    "$receiver_root/usr/local/libexec/teslatlas-edge/teslatlas-fleet-telemetry"
install -m 0644 "$root/packaging/macos/uk.co.magrathean.teslatlas-fleet-telemetry.plist" \
    "$receiver_root/Library/Application Support/Teslatlas Edge/launchagents/uk.co.magrathean.teslatlas-fleet-telemetry.plist"

pkgbuild --root "$core_root" --identifier uk.co.magrathean.teslatlas.edge.core \
    --version "$version" --ownership recommended "$stage/teslatlas-edge.core.pkg"
pkgbuild --root "$receiver_root" --identifier uk.co.magrathean.teslatlas.edge.receiver \
    --version "$version" --ownership recommended "$stage/teslatlas-edge.receiver.pkg"
sed "s/@VERSION@/$version/g" "$root/packaging/macos/Distribution.xml" > "$stage/Distribution.xml"
mkdir -p "$(dirname -- "$output")"
productbuild --distribution "$stage/Distribution.xml" \
    --package-path "$stage" --resources "$root/packaging/macos/resources" "$output"
