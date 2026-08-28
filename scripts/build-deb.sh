#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

usage() {
    echo "usage: $0 --binary PATH --version VERSION --output PATH --legal-bundle PATH [--architecture amd64|arm64] [--command-proxy-binary PATH --fleet-telemetry-binary PATH --go-proxy-evidence PATH --fleet-telemetry-evidence PATH]" >&2
    exit 64
}

binary=
command_proxy_binary=
fleet_telemetry_binary=
version=
output=
architecture=
legal_bundle=
go_proxy_evidence=
fleet_telemetry_evidence=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) [ "$#" -ge 2 ] || usage; binary=$2; shift 2 ;;
        --command-proxy-binary) [ "$#" -ge 2 ] || usage; command_proxy_binary=$2; shift 2 ;;
        --fleet-telemetry-binary) [ "$#" -ge 2 ] || usage; fleet_telemetry_binary=$2; shift 2 ;;
        --version) [ "$#" -ge 2 ] || usage; version=$2; shift 2 ;;
        --output) [ "$#" -ge 2 ] || usage; output=$2; shift 2 ;;
        --architecture) [ "$#" -ge 2 ] || usage; architecture=$2; shift 2 ;;
        --legal-bundle) [ "$#" -ge 2 ] || usage; legal_bundle=$2; shift 2 ;;
        --go-proxy-evidence) [ "$#" -ge 2 ] || usage; go_proxy_evidence=$2; shift 2 ;;
        --fleet-telemetry-evidence) [ "$#" -ge 2 ] || usage; fleet_telemetry_evidence=$2; shift 2 ;;
        *) usage ;;
    esac
done

[ -n "$binary" ] && [ -n "$version" ] && [ -n "$output" ] && [ -n "$legal_bundle" ] || usage
case "${command_proxy_binary:+set}:${fleet_telemetry_binary:+set}" in
    :) include_fleet_sidecars=false
       [ -z "$go_proxy_evidence" ] && [ -z "$fleet_telemetry_evidence" ] \
           || { echo "sidecar evidence requires sidecar binaries" >&2; exit 64; } ;;
    set:set) include_fleet_sidecars=true
       [ -n "$go_proxy_evidence" ] && [ -n "$fleet_telemetry_evidence" ] \
           || { echo "sidecar binaries require both sidecar evidence directories" >&2; exit 64; } ;;
    *) echo "both sidecar binaries must be supplied together" >&2; exit 64 ;;
esac
regular_binary_path() {
    input=$1
    label=$2
    [ -f "$input" ] && [ ! -L "$input" ] \
        || { echo "$label is not a regular file" >&2; exit 65; }
    input_directory=$(CDPATH='' cd -- "$(dirname -- "$input")" && pwd)
    printf '%s/%s\n' "$input_directory" "$(basename -- "$input")"
}
require_hub_version() {
    version_binary=$1
    expected_version=$2
    version_output=$(
        version_status=0
        "$version_binary" --version 2>&1 || version_status=$?
        printf '%s' "__TESLATLAS_HUB_VERSION_STATUS_${version_status}__"
    )
    expected_output=$(printf 'teslatlas-hub %s\n%s' \
        "$expected_version" '__TESLATLAS_HUB_VERSION_STATUS_0__')
    [ "$version_output" = "$expected_output" ]
}
binary=$(regular_binary_path "$binary" 'binary')
if [ "$include_fleet_sidecars" = true ]; then
    command_proxy_binary=$(regular_binary_path "$command_proxy_binary" 'command proxy binary')
    fleet_telemetry_binary=$(regular_binary_path "$fleet_telemetry_binary" 'Fleet Telemetry binary')
fi
sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "sha256sum or shasum is required" >&2
        exit 69
    fi
}
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
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
command -v python3 >/dev/null 2>&1 || {
    echo "python3 is required" >&2
    exit 69
}
legal_helper="$root/scripts/legal-bundle.py"
[ -f "$legal_helper" ] && [ ! -L "$legal_helper" ] \
    || { echo "dependency legal bundle verifier is missing or unsafe" >&2; exit 65; }
verify_legal_bundle() {
    if [ "$include_fleet_sidecars" = true ]; then
        python3 "$legal_helper" --repo "$root" --verify-dir "$legal_bundle" \
            --go-proxy-evidence "$go_proxy_evidence" \
            --fleet-telemetry-evidence "$fleet_telemetry_evidence"
    else
        python3 "$legal_helper" --repo "$root" --verify-dir "$legal_bundle"
    fi
}
verify_legal_bundle >/dev/null \
    || { echo "dependency legal bundle is invalid" >&2; exit 65; }
if [ "$include_fleet_sidecars" = true ]; then
    sidecar_lock="$root/packaging/linux/sidecar-sha256.lock"
    [ -f "$sidecar_lock" ] && [ ! -L "$sidecar_lock" ] \
        || { echo "reviewed Linux sidecar lock is missing or unsafe" >&2; exit 65; }
    lock_values=$(awk -v wanted="$architecture" '
        /^[[:space:]]*(#|$)/ { next }
        NF != 3 || ($1 != "amd64" && $1 != "arm64") { invalid = 1; next }
        seen[$1]++
        $1 == wanted { selected = $2 " " $3 }
        END {
            if (invalid || seen["amd64"] != 1 || seen["arm64"] != 1 || selected == "") exit 1
            print selected
        }
    ' "$sidecar_lock") || {
        echo "reviewed Linux sidecar lock is invalid" >&2
        exit 65
    }
    command_proxy_sha256=${lock_values%% *}
    fleet_telemetry_sha256=${lock_values#* }
    [ "$command_proxy_sha256" != "$lock_values" ] \
        && [ "$fleet_telemetry_sha256" != "$lock_values" ] \
        && [ "${fleet_telemetry_sha256#* }" = "$fleet_telemetry_sha256" ] \
        || { echo "reviewed Linux sidecar lock is invalid" >&2; exit 65; }
    for reviewed_digest in "$command_proxy_sha256" "$fleet_telemetry_sha256"; do
        printf '%s\n' "$reviewed_digest" | grep -Eq '^[0-9a-f]{64}$' \
            || { echo "reviewed Linux sidecar lock is invalid" >&2; exit 65; }
    done
    [ "$(sha256_file "$command_proxy_binary")" = "$command_proxy_sha256" ] \
        || { echo "command proxy does not match the reviewed Linux build" >&2; exit 65; }
    [ "$(sha256_file "$fleet_telemetry_binary")" = "$fleet_telemetry_sha256" ] \
        || { echo "Fleet Telemetry receiver does not match the reviewed Linux build" >&2; exit 65; }
fi
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
    || { echo "version must be semver with an optional prerelease" >&2; exit 65; }
[ -x "$binary" ] || {
    echo "binary must be executable" >&2
    exit 65
}
require_hub_version "$binary" "$version" || {
    echo "binary version does not match package version" >&2
    exit 65
}
case "$version" in
    *-*) debian_version=${version%%-*}\~${version#*-}-1 ;;
    *) debian_version=$version-1 ;;
esac
elf_header_field() {
    elf_binary=$1
    elf_field=$2
    LC_ALL=C readelf -h "$elf_binary" 2>/dev/null | awk -F: -v field="$elf_field" '
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

validate_elf_binary() {
    elf_binary=$1
    elf_label=$2
    actual_class=$(elf_header_field "$elf_binary" 'Class')
    actual_data=$(elf_header_field "$elf_binary" 'Data')
    actual_os_abi=$(elf_header_field "$elf_binary" 'OS/ABI')
    actual_machine=$(elf_header_field "$elf_binary" 'Machine')
    actual_type=$(elf_header_field "$elf_binary" 'Type')
    [ "$actual_class" = 'ELF64' ] || {
        echo "$elf_label ELF class is ${actual_class:-unknown}, expected ELF64" >&2
        exit 65
    }
    [ "$actual_data" = "2's complement, little endian" ] || {
        echo "$elf_label ELF byte order is ${actual_data:-unknown}, expected little endian" >&2
        exit 65
    }
    case "$actual_os_abi" in
        'UNIX - System V'|'UNIX - GNU') ;;
        *) echo "$elf_label OS ABI is ${actual_os_abi:-unknown}, expected UNIX - System V or UNIX - GNU" >&2; exit 65 ;;
    esac
    [ "$actual_machine" = "$expected_machine" ] || {
        echo "$elf_label machine is ${actual_machine:-unknown}, expected $expected_machine for $architecture" >&2
        exit 65
    }
    case "$actual_type" in
        'EXEC (Executable file)'|'DYN (Position-Independent Executable file)') ;;
        *) echo "$elf_label ELF type is ${actual_type:-unknown}, expected an executable" >&2; exit 65 ;;
    esac

    actual_interpreter=$(LC_ALL=C readelf -l "$elf_binary" 2>/dev/null | awk '
    /Requesting program interpreter:/ {
        value = $0
        sub(/^.*Requesting program interpreter: /, "", value)
        sub(/\].*$/, "", value)
        print value
        exit
    }
')
    validated_binary_is_dynamic=false
    if [ -n "$actual_interpreter" ]; then
        validated_binary_is_dynamic=true
        interpreter_ok=false
        for expected_interpreter in $expected_interpreters; do
            if [ "$actual_interpreter" = "$expected_interpreter" ]; then
                interpreter_ok=true
                break
            fi
        done
        [ "$interpreter_ok" = true ] || {
            echo "$elf_label interpreter is $actual_interpreter, unsupported for $architecture" >&2
            exit 65
        }
    elif LC_ALL=C readelf -d "$elf_binary" 2>/dev/null | awk '/\(NEEDED\)/ { found = 1 } END { exit !found }'; then
        validated_binary_is_dynamic=true
        echo "$elf_label is dynamic but has no program interpreter" >&2
        exit 65
    fi
}

validate_elf_binary "$binary" 'binary'
binary_is_dynamic=$validated_binary_is_dynamic
command_proxy_is_dynamic=false
fleet_telemetry_is_dynamic=false
if [ "$include_fleet_sidecars" = true ]; then
    validate_elf_binary "$command_proxy_binary" 'command proxy binary'
    command_proxy_is_dynamic=$validated_binary_is_dynamic
    validate_elf_binary "$fleet_telemetry_binary" 'Fleet Telemetry binary'
    fleet_telemetry_is_dynamic=$validated_binary_is_dynamic
fi

stage=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-deb.XXXXXX")
trap 'find "$stage" -depth -delete' EXIT HUP INT TERM
package_root="$stage/debian/teslatlas-hub"

install -D -m 0755 "$binary" "$package_root/usr/bin/teslatlas-hub"
if [ "$include_fleet_sidecars" = true ]; then
    install -D -m 0755 "$command_proxy_binary" \
        "$package_root/usr/lib/teslatlas-hub/tesla-http-proxy"
    install -D -m 0755 "$fleet_telemetry_binary" \
        "$package_root/usr/lib/teslatlas-hub/fleet-telemetry"
    [ "$(sha256_file "$package_root/usr/lib/teslatlas-hub/tesla-http-proxy")" \
        = "$command_proxy_sha256" ] \
        || { echo "staged command proxy digest changed" >&2; exit 65; }
    [ "$(sha256_file "$package_root/usr/lib/teslatlas-hub/fleet-telemetry")" \
        = "$fleet_telemetry_sha256" ] \
        || { echo "staged Fleet Telemetry receiver digest changed" >&2; exit 65; }
    mkdir -p "$package_root/usr/share/doc/teslatlas-hub"
    printf '%s  %s\n%s  %s\n' \
        "$command_proxy_sha256" tesla-http-proxy \
        "$fleet_telemetry_sha256" fleet-telemetry \
        > "$package_root/usr/share/doc/teslatlas-hub/SIDECAR_SHA256SUMS"
    install -m 0644 "$sidecar_lock" \
        "$package_root/usr/share/doc/teslatlas-hub/SIDECAR_BUILD_LOCK"
fi
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
if [ "$binary_is_dynamic" = true ] \
    || [ "$command_proxy_is_dynamic" = true ] \
    || [ "$fleet_telemetry_is_dynamic" = true ]; then
    command -v dpkg-shlibdeps >/dev/null 2>&1 || {
        echo "dpkg-shlibdeps is required for dynamic binaries; install dpkg-dev" >&2
        exit 69
    }
    set -- -O
    [ "$binary_is_dynamic" = false ] \
        || set -- "$@" -e "debian/teslatlas-hub/usr/bin/teslatlas-hub"
    [ "$command_proxy_is_dynamic" = false ] \
        || set -- "$@" -e "debian/teslatlas-hub/usr/lib/teslatlas-hub/tesla-http-proxy"
    [ "$fleet_telemetry_is_dynamic" = false ] \
        || set -- "$@" -e "debian/teslatlas-hub/usr/lib/teslatlas-hub/fleet-telemetry"
    if ! shlib_substvars=$(CDPATH='' cd -- "$stage" && dpkg-shlibdeps "$@"); then
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
install -D -m 0644 "$root/packaging/linux/teslatlas-hub-terminal-failure.target" \
    "$package_root/lib/systemd/system/teslatlas-hub-terminal-failure.target"
install -D -m 0644 "$root/packaging/linux/config.toml" \
    "$package_root/etc/teslatlas-hub/config.toml"
if [ "$include_fleet_sidecars" = true ]; then
    install -D -m 0644 "$root/packaging/linux/teslatlas-command-proxy.service" \
        "$package_root/lib/systemd/system/teslatlas-command-proxy.service"
    install -D -m 0644 "$root/packaging/linux/teslatlas-fleet-telemetry.service" \
        "$package_root/lib/systemd/system/teslatlas-fleet-telemetry.service"
    install -D -m 0644 "$root/packaging/linux/command-proxy.env" \
        "$package_root/etc/teslatlas-hub/command-proxy.env"
    install -D -m 0644 "$root/packaging/linux/fleet-telemetry.json" \
        "$package_root/etc/teslatlas-hub/fleet-telemetry.json"
fi
install -D -m 0644 "$root/LICENSE" "$package_root/usr/share/doc/teslatlas-hub/copyright"
install -D -m 0644 "$root/NOTICE" "$package_root/usr/share/doc/teslatlas-hub/NOTICE"
install -D -m 0644 "$root/THIRD_PARTY_NOTICES.md" \
    "$package_root/usr/share/doc/teslatlas-hub/THIRD_PARTY_NOTICES.md"
install -D -m 0644 "$root/PROVENANCE.md" \
    "$package_root/usr/share/doc/teslatlas-hub/PROVENANCE.md"
install -D -m 0644 "$root/ADDITIONAL_TERMS.md" \
    "$package_root/usr/share/doc/teslatlas-hub/ADDITIONAL_TERMS.md"
install -D -m 0644 "$root/SOURCE_AVAILABILITY.md" \
    "$package_root/usr/share/doc/teslatlas-hub/SOURCE_AVAILABILITY.md"
install -D -m 0644 "$root/RELEASE_VERIFICATION.md" \
    "$package_root/usr/share/doc/teslatlas-hub/RELEASE_VERIFICATION.md"
dependency_legal="$package_root/usr/share/doc/teslatlas-hub/dependency-legal"
mkdir -p "$dependency_legal"
for legal_component in RUST_THIRD_PARTY_NOTICES.generated.md \
    rust-dependency-inventory.json rust-sbom.spdx.json legal-bundle-manifest.json; do
    install -m 0644 "$legal_bundle/$legal_component" "$dependency_legal/$legal_component"
done
if [ "$include_fleet_sidecars" = true ]; then
    for legal_component in FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md \
        GO_THIRD_PARTY_NOTICES.generated.md fleet-telemetry-bridge-lock.json \
        fleet-telemetry-dependency-inventory.json fleet-telemetry-legal-lock.json \
        fleet-telemetry-license-material.tar.gz fleet-telemetry-sbom.spdx.json \
        go-dependency-inventory.json go-sbom.spdx.json; do
        install -m 0644 "$legal_bundle/$legal_component" "$dependency_legal/$legal_component"
    done
fi
verify_legal_bundle >/dev/null \
    || { echo "dependency legal bundle changed while packaging" >&2; exit 65; }
for legal_component in "$legal_bundle"/*; do
    component_name=$(basename -- "$legal_component")
    cmp "$legal_component" "$dependency_legal/$component_name" >/dev/null \
        || { echo "staged dependency legal component changed: $component_name" >&2; exit 65; }
done
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
if [ "$include_fleet_sidecars" = true ]; then
    printf '%s\n' \
        '/etc/teslatlas-hub/command-proxy.env' \
        '/etc/teslatlas-hub/fleet-telemetry.json' >> "$package_root/DEBIAN/conffiles"
fi

mkdir -p "$(dirname -- "$output")"
dpkg-deb --root-owner-group --build "$package_root" "$output"
