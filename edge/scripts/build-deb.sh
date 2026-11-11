#!/bin/sh
set -eu

usage() {
    echo "usage: $0 --binary PATH --receiver-binary PATH --version VERSION --output PATH [--architecture amd64|arm64]" >&2
    exit 64
}

binary=
receiver_binary=
version=
output=
architecture=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) [ "$#" -ge 2 ] || usage; binary=$2; shift 2 ;;
        --receiver-binary) [ "$#" -ge 2 ] || usage; receiver_binary=$2; shift 2 ;;
        --version) [ "$#" -ge 2 ] || usage; version=$2; shift 2 ;;
        --output) [ "$#" -ge 2 ] || usage; output=$2; shift 2 ;;
        --architecture) [ "$#" -ge 2 ] || usage; architecture=$2; shift 2 ;;
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

if [ -z "$architecture" ]; then
    command -v dpkg >/dev/null 2>&1 || {
        echo "--architecture is required when dpkg is unavailable" >&2
        exit 69
    }
    architecture=$(dpkg --print-architecture)
fi
case "$architecture" in
    amd64)
        expected_machine='Advanced Micro Devices X86-64'
        expected_interpreter='/lib64/ld-linux-x86-64.so.2'
        ;;
    arm64)
        expected_machine='AArch64'
        expected_interpreter='/lib/ld-linux-aarch64.so.1'
        ;;
    *)
        echo "unsupported Debian architecture: $architecture" >&2
        exit 65
        ;;
esac

command -v readelf >/dev/null 2>&1 || {
    echo "readelf is required; install binutils" >&2
    exit 69
}
command -v dpkg-deb >/dev/null 2>&1 || {
    echo "dpkg-deb is required" >&2
    exit 69
}

elf_field() {
    elf_binary=$1
    wanted_field=$2
    LC_ALL=C readelf -h "$elf_binary" 2>/dev/null | awk -F: -v field="$wanted_field" '
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

validate_elf() {
    elf_binary=$1
    label=$2
    [ "$(elf_field "$elf_binary" Class)" = ELF64 ] || {
        echo "$label is not an ELF64 executable" >&2
        exit 65
    }
    [ "$(elf_field "$elf_binary" Machine)" = "$expected_machine" ] || {
        echo "$label architecture does not match $architecture" >&2
        exit 65
    }
    case "$(elf_field "$elf_binary" Type)" in
        'EXEC (Executable file)'|'DYN (Position-Independent Executable file)') ;;
        *) echo "$label is not an executable ELF" >&2; exit 65 ;;
    esac
    interpreter=$(LC_ALL=C readelf -l "$elf_binary" 2>/dev/null | awk '
        /Requesting program interpreter:/ {
            value = $0
            sub(/^.*Requesting program interpreter: /, "", value)
            sub(/\].*$/, "", value)
            print value
            exit
        }
    ')
    [ -z "$interpreter" ] || [ "$interpreter" = "$expected_interpreter" ] || {
        echo "$label uses an unsupported program interpreter" >&2
        exit 65
    }
}

version_output=$("$binary" --version 2>&1) || {
    echo "Edge binary version query failed" >&2
    exit 65
}
[ "$version_output" = "teslatlas-edge $version" ] || {
    echo "Edge binary version does not match package version" >&2
    exit 65
}
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' || {
    echo "version must be semver with an optional prerelease" >&2
    exit 65
}
[ ! -e "$output" ] && [ ! -L "$output" ] || {
    echo "output already exists" >&2
    exit 65
}

validate_elf "$binary" 'Edge binary'
validate_elf "$receiver_binary" 'receiver binary'

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-edge-deb.XXXXXX")
trap 'find "$stage" -depth -delete' EXIT HUP INT TERM
package_root="$stage/teslatlas-edge"

install -d -m 0755 \
    "$package_root/DEBIAN" \
    "$package_root/etc/teslatlas-edge" \
    "$package_root/lib/systemd/system" \
    "$package_root/usr/bin" \
    "$package_root/usr/lib/teslatlas-edge" \
    "$package_root/usr/share/doc/teslatlas-edge"
install -m 0755 "$binary" "$package_root/usr/bin/teslatlas-edge"
install -m 0755 "$receiver_binary" "$package_root/usr/lib/teslatlas-edge/fleet-telemetry"
install -m 0755 "$root/scripts/run-with-spool-format-guard.sh" \
    "$package_root/usr/lib/teslatlas-edge/run-with-spool-format-guard.sh"
install -m 0644 "$root/packaging/linux/teslatlas-edge.service" \
    "$package_root/lib/systemd/system/teslatlas-edge.service"
install -m 0644 "$root/packaging/linux/teslatlas-fleet-telemetry.service" \
    "$package_root/lib/systemd/system/teslatlas-fleet-telemetry.service"
install -m 0644 "$root/packaging/config.toml.example" \
    "$package_root/etc/teslatlas-edge/config.toml"
install -m 0644 "$root/packaging/fleet-telemetry.json.example" \
    "$package_root/etc/teslatlas-edge/fleet-telemetry.json"
install -m 0644 "$root/LICENSE" "$package_root/usr/share/doc/teslatlas-edge/copyright"
install -m 0644 "$root/docs/legal/third-party-notices.md" \
    "$package_root/usr/share/doc/teslatlas-edge/THIRD_PARTY_NOTICES.md"
install -m 0644 "$root/docs/operations/native-installation.md" \
    "$package_root/usr/share/doc/teslatlas-edge/NATIVE_INSTALLATION.md"
install -m 0644 "$root/docs/operations/upgrade-backup-recovery.md" \
    "$package_root/usr/share/doc/teslatlas-edge/UPGRADE_BACKUP_RECOVERY.md"

case "$version" in
    *-*) debian_version=${version%%-*}~${version#*-}-1 ;;
    *) debian_version=$version-1 ;;
esac
sed \
    -e "s/@VERSION@/$debian_version/g" \
    -e "s/@ARCHITECTURE@/$architecture/g" \
    "$root/packaging/linux/control.in" > "$package_root/DEBIAN/control"
for hook in preinst postinst prerm postrm; do
    install -m 0755 "$root/packaging/linux/$hook" "$package_root/DEBIAN/$hook"
done
printf '%s\n' \
    '/etc/teslatlas-edge/config.toml' \
    '/etc/teslatlas-edge/fleet-telemetry.json' > "$package_root/DEBIAN/conffiles"

mkdir -p "$(dirname -- "$output")"
dpkg-deb --root-owner-group --build "$package_root" "$output"
