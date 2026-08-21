#!/bin/sh
set -eu

usage() {
    echo "usage: $0 --binary PATH --version VERSION --output PATH" >&2
    exit 64
}

binary=
version=
output=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) binary=${2-}; shift 2 ;;
        --version) version=${2-}; shift 2 ;;
        --output) output=${2-}; shift 2 ;;
        *) usage ;;
    esac
done

[ -n "$binary" ] && [ -n "$version" ] && [ -n "$output" ] || usage
[ -f "$binary" ] || { echo "binary is not a regular file" >&2; exit 65; }
case "$version" in
    *[!0-9A-Za-z.+:~\-]*) echo "invalid Debian version" >&2; exit 65 ;;
esac

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-deb.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
package_root="$stage/root"

install -D -m 0755 "$binary" "$package_root/usr/bin/teslatlas-hub"
install -D -m 0644 "$root/packaging/linux/teslatlas-hub.service" \
    "$package_root/lib/systemd/system/teslatlas-hub.service"
install -D -m 0644 "$root/packaging/linux/config.toml" \
    "$package_root/etc/teslatlas-hub/config.toml"
install -D -m 0755 "$root/packaging/linux/postinst" "$package_root/DEBIAN/postinst"
install -D -m 0755 "$root/packaging/linux/prerm" "$package_root/DEBIAN/prerm"
sed "s/@VERSION@/$version/g" "$root/packaging/linux/control.in" > "$package_root/DEBIAN/control"
printf '%s\n' '/etc/teslatlas-hub/config.toml' > "$package_root/DEBIAN/conffiles"

mkdir -p "$(dirname -- "$output")"
dpkg-deb --root-owner-group --build "$package_root" "$output"
