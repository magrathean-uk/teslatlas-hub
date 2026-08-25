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
[ -f "$binary" ] && [ ! -L "$binary" ] \
    || { echo "binary is not a regular file" >&2; exit 65; }
binary_directory=$(CDPATH='' cd -- "$(dirname -- "$binary")" && pwd)
binary="$binary_directory/$(basename -- "$binary")"
command -v readelf >/dev/null 2>&1 || {
    echo "readelf is required; install binutils" >&2
    exit 69
}
if [ -z "$architecture" ]; then
    architecture=$(dpkg --print-architecture)
fi
case "$architecture" in
    amd64)
        expected_machine='Advanced Micro Devices X86-64'
        expected_interpreters='/lib64/ld-linux-x86-64.so.2'
        ;;
    arm64)
        expected_machine='AArch64'
        expected_interpreters='/lib/ld-linux-aarch64.so.1'
        ;;
    *) echo "unsupported Debian architecture: $architecture" >&2; exit 65 ;;
esac
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
    || { echo "version must be semver with an optional prerelease" >&2; exit 65; }
case "$version" in
    *-*) debian_version=${version%%-*}\~${version#*-}-1 ;;
    *) debian_version=$version-1 ;;
esac
elf_header_field() {
    LC_ALL=C readelf -h "$binary" 2>/dev/null | awk -F: -v field="$1" '
        {
            key = $1
            sub(/^[[:space:]]+/, "", key)
            sub(/[[:space:]]+$/, "", key)
            if (key == field) {
                value = $2
                sub(/^[[:space:]]+/, "", value)
                sub(/[[:space:]]+$/, "", value)
                print value
                exit
            }
        }
    '
}

actual_class=$(elf_header_field 'Class')
actual_data=$(elf_header_field 'Data')
actual_os_abi=$(elf_header_field 'OS/ABI')
actual_machine=$(elf_header_field 'Machine')
actual_type=$(elf_header_field 'Type')
[ "$actual_class" = 'ELF64' ] || {
    echo "binary ELF class is ${actual_class:-unknown}, expected ELF64" >&2
    exit 65
}
[ "$actual_data" = "2's complement, little endian" ] || {
    echo "binary ELF byte order is ${actual_data:-unknown}, expected little endian" >&2
    exit 65
}
case "$actual_os_abi" in
    'UNIX - System V'|'UNIX - GNU') ;;
    *) echo "binary OS ABI is ${actual_os_abi:-unknown}, expected UNIX - System V or UNIX - GNU" >&2; exit 65 ;;
esac
[ "$actual_machine" = "$expected_machine" ] || {
    echo "binary machine is ${actual_machine:-unknown}, expected $expected_machine for $architecture" >&2
    exit 65
}
case "$actual_type" in
    'EXEC (Executable file)'|'DYN (Position-Independent Executable file)') ;;
    *) echo "binary ELF type is ${actual_type:-unknown}, expected an executable" >&2; exit 65 ;;
esac

actual_interpreter=$(LC_ALL=C readelf -l "$binary" 2>/dev/null | awk '
    /Requesting program interpreter:/ {
        value = $0
        sub(/^.*Requesting program interpreter: /, "", value)
        sub(/\].*$/, "", value)
        print value
        exit
    }
')
binary_is_dynamic=false
if [ -n "$actual_interpreter" ]; then
    binary_is_dynamic=true
    interpreter_ok=false
    for expected_interpreter in $expected_interpreters; do
        if [ "$actual_interpreter" = "$expected_interpreter" ]; then
            interpreter_ok=true
            break
        fi
    done
    [ "$interpreter_ok" = true ] || {
        echo "binary interpreter is $actual_interpreter, unsupported for $architecture" >&2
        exit 65
    }
elif LC_ALL=C readelf -d "$binary" 2>/dev/null | awk '/\(NEEDED\)/ { found = 1 } END { exit !found }'; then
    binary_is_dynamic=true
    echo "dynamic binary has no program interpreter" >&2
    exit 65
fi

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-deb.XXXXXX")
trap 'find "$stage" -depth -delete' EXIT HUP INT TERM
package_root="$stage/debian/teslatlas-hub"

install -D -m 0755 "$binary" "$package_root/usr/bin/teslatlas-hub"
mkdir -p "$stage/debian"
printf '%s\n' \
    'Source: teslatlas-hub' \
    'Section: utils' \
    'Priority: optional' \
    'Maintainer: Magrathean UK Ltd <contact@magrathean.uk>' \
    '' \
    'Package: teslatlas-hub' \
    "Architecture: $architecture" \
    'Description: Self-hosted multi-car Tesla telemetry hub' > "$stage/debian/control"
runtime_dependencies=
if [ "$binary_is_dynamic" = true ]; then
    command -v dpkg-shlibdeps >/dev/null 2>&1 || {
        echo "dpkg-shlibdeps is required for dynamic binaries; install dpkg-dev" >&2
        exit 69
    }
    if ! shlib_substvars=$(CDPATH='' cd -- "$stage" && \
        dpkg-shlibdeps -O -e "debian/teslatlas-hub/usr/bin/teslatlas-hub"); then
        echo "failed to determine shared-library dependencies" >&2
        exit 65
    fi
    runtime_dependencies=$(printf '%s\n' "$shlib_substvars" | awk '
        /^shlibs:Depends=/ {
            if (found) {
                invalid = 1
                next
            }
            found = 1
            sub(/^shlibs:Depends=/, "")
            dependencies = $0
        }
        END {
            if (!found || invalid) {
                exit 1
            }
            print dependencies
        }
') || {
        echo "dpkg-shlibdeps returned invalid dependency output" >&2
        exit 65
    }
fi
runtime_dependency_suffix=
if [ -n "$runtime_dependencies" ]; then
    runtime_dependency_suffix=", $runtime_dependencies"
fi
escaped_runtime_dependency_suffix=$(printf '%s' "$runtime_dependency_suffix" | sed 's/[&|\\]/\\&/g')

install -D -m 0644 "$root/packaging/linux/teslatlas-hub.service" \
    "$package_root/lib/systemd/system/teslatlas-hub.service"
install -D -m 0644 "$root/packaging/linux/config.toml" \
    "$package_root/etc/teslatlas-hub/config.toml"
install -D -m 0644 "$root/LICENSE" "$package_root/usr/share/doc/teslatlas-hub/copyright"
install -D -m 0644 "$root/NOTICE" "$package_root/usr/share/doc/teslatlas-hub/NOTICE"
install -D -m 0644 "$root/THIRD_PARTY_NOTICES.md" \
    "$package_root/usr/share/doc/teslatlas-hub/THIRD_PARTY_NOTICES.md"
install -D -m 0644 "$root/PROVENANCE.md" \
    "$package_root/usr/share/doc/teslatlas-hub/PROVENANCE.md"
install -D -m 0755 "$root/packaging/linux/preinst" "$package_root/DEBIAN/preinst"
install -D -m 0755 "$root/packaging/linux/postinst" "$package_root/DEBIAN/postinst"
install -D -m 0755 "$root/packaging/linux/prerm" "$package_root/DEBIAN/prerm"
install -D -m 0755 "$root/packaging/linux/postrm" "$package_root/DEBIAN/postrm"
sed \
    -e "s/@VERSION@/$debian_version/g" \
    -e "s/@ARCHITECTURE@/$architecture/g" \
    -e "s|@RUNTIME_DEPENDS@|$escaped_runtime_dependency_suffix|g" \
    "$root/packaging/linux/control.in" > "$package_root/DEBIAN/control"
printf '%s\n' '/etc/teslatlas-hub/config.toml' > "$package_root/DEBIAN/conffiles"

mkdir -p "$(dirname -- "$output")"
dpkg-deb --root-owner-group --build "$package_root" "$output"
