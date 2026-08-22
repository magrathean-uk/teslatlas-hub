#!/bin/sh
set -eu

usage() {
    echo "usage: $0 --binary PATH --version VERSION --output PATH [--architecture amd64|arm64]" >&2
    exit 64
}

binary=
version=
output=
architecture=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) binary=${2-}; shift 2 ;;
        --version) version=${2-}; shift 2 ;;
        --output) output=${2-}; shift 2 ;;
        --architecture) architecture=${2-}; shift 2 ;;
        *) usage ;;
    esac
done

[ -n "$binary" ] && [ -n "$version" ] && [ -n "$output" ] || usage
[ -f "$binary" ] || { echo "binary is not a regular file" >&2; exit 65; }
command -v readelf >/dev/null 2>&1 || {
    echo "readelf is required; install binutils" >&2
    exit 69
}
if [ -z "$architecture" ]; then
    architecture=$(dpkg --print-architecture)
fi
case "$architecture" in
    amd64) expected_machine='Advanced Micro Devices X86-64' ;;
    arm64) expected_machine='AArch64' ;;
    *) echo "unsupported Debian architecture: $architecture" >&2; exit 65 ;;
esac
case "$version" in
    *[!0-9A-Za-z.+:~\-]*) echo "invalid Debian version" >&2; exit 65 ;;
esac
actual_machine=$(LC_ALL=C readelf -h "$binary" 2>/dev/null | awk -F: '
    $1 ~ /^[[:space:]]*Machine$/ {
        sub(/^[[:space:]]+/, "", $2)
        print $2
        exit
    }
')
[ "$actual_machine" = "$expected_machine" ] || {
    echo "binary machine is ${actual_machine:-unknown}, expected $expected_machine for $architecture" >&2
    exit 65
}

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-deb.XXXXXX")
trap 'find "$stage" -depth -delete' EXIT HUP INT TERM
package_root="$stage/root"

install -D -m 0755 "$binary" "$package_root/usr/bin/teslatlas-hub"
install -D -m 0644 "$root/packaging/linux/teslatlas-hub.service" \
    "$package_root/lib/systemd/system/teslatlas-hub.service"
install -D -m 0644 "$root/packaging/linux/config.toml" \
    "$package_root/etc/teslatlas-hub/config.toml"
install -D -m 0755 "$root/packaging/linux/postinst" "$package_root/DEBIAN/postinst"
install -D -m 0755 "$root/packaging/linux/prerm" "$package_root/DEBIAN/prerm"
sed \
    -e "s/@VERSION@/$version/g" \
    -e "s/@ARCHITECTURE@/$architecture/g" \
    "$root/packaging/linux/control.in" > "$package_root/DEBIAN/control"
printf '%s\n' '/etc/teslatlas-hub/config.toml' > "$package_root/DEBIAN/conffiles"

mkdir -p "$(dirname -- "$output")"
dpkg-deb --root-owner-group --build "$package_root" "$output"
