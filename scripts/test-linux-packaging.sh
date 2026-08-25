#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-package-test.XXXXXX")
trap 'find "$test_root" -depth -delete' EXIT HUP INT TERM
maintainer_bin="$test_root/maintainer-bin"
elf_bin="$test_root/elf-bin"
systemd_dir="$test_root/systemd"
systemctl_log="$test_root/systemctl.log"
hub_status_log="$test_root/hub-status.log"
maintainer_state="$test_root/maintainer.state"
upgrade_backup="$test_root/upgrade-backup"
config_file="$test_root/config.toml"
installed_binary="$test_root/teslatlas-hub"
installed_unit="$test_root/teslatlas-hub.service"
schema_upgraded="$test_root/schema-upgraded"
captured_control="$test_root/control"
mkdir -p "$maintainer_bin" "$elf_bin" "$systemd_dir"
export TESLATLAS_HUB_MAINTAINER_STATE_FILE="$maintainer_state"
export TESLATLAS_HUB_HEALTH_ATTEMPTS=2
export TESLATLAS_HUB_HEALTH_DELAY_SECONDS=0
export TESLATLAS_HUB_HEALTH_TIMEOUT_SECONDS=1
export TESLATLAS_HUB_RUNUSER="$maintainer_bin/runuser"
export TESLATLAS_HUB_TIMEOUT="$maintainer_bin/timeout"
export TESLATLAS_HUB_UPGRADE_BACKUP_DIR="$upgrade_backup"
export TESLATLAS_HUB_CONFIG="$config_file"
export TESLATLAS_HUB_BINARY="$installed_binary"
export TESLATLAS_HUB_UNIT_FILE="$installed_unit"
export HUB_SCHEMA_UPGRADED_FILE="$schema_upgraded"

grep -Fqx 'StateDirectoryMode=0700' "$root/packaging/linux/teslatlas-hub.service" || {
    echo 'test-linux-packaging: systemd state directory is not private' >&2
    exit 1
}

fail() {
    echo "test-linux-packaging: $*" >&2
    if [ -f "$test_root/package-output" ]; then
        sed -n '1,20p' "$test_root/package-output" >&2
    fi
    exit 1
}

require_log() {
    grep -Fqx "$1" "$systemctl_log" || fail "missing systemctl call: $1"
}

for utility in getent adduser chown chmod; do
    printf '%s\n' '#!/bin/sh' 'exit 0' > "$maintainer_bin/$utility"
    chmod 0755 "$maintainer_bin/$utility"
done
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'case " $* " in *" -d "*) exit 0 ;; esac' \
    'previous=' \
    'last=' \
    'for value in "$@"; do previous=$last; last=$value; done' \
    'if [ "$previous" = /dev/null ]; then' \
    '  mkdir -p "$(dirname -- "$last")"' \
    '  : > "$last"' \
    'elif [ -f "$previous" ]; then' \
    '  mkdir -p "$(dirname -- "$last")"' \
    '  cp "$previous" "$last"' \
    'fi' > "$maintainer_bin/install"
chmod 0755 "$maintainer_bin/install"
printf '%s\n' \
    '#!/bin/sh' \
    'shift' \
    'exec "$@"' > "$maintainer_bin/timeout"
chmod 0755 "$maintainer_bin/timeout"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "%s\\n" "$*" >> "$HUB_STATUS_LOG"' \
    'case " $* " in' \
    '  *" bootstrap "*) [ "${HUB_BOOTSTRAP_READY:-1}" = 1 ] || exit 1; : > "$HUB_SCHEMA_UPGRADED_FILE"; exit 0 ;;' \
    '  *" preflight "*)' \
    '    [ "${HUB_PREFLIGHT_READY:-1}" = 1 ] || exit 1' \
    '    if [ "${HUB_SCHEMA_UPGRADE:-0}" = 1 ] && [ ! -f "$HUB_SCHEMA_UPGRADED_FILE" ]; then exit 1; fi' \
    '    exit 0' \
    '    ;;' \
    'esac' \
    'if [ "${HUB_READY:-1}" = 1 ]; then' \
    '  printf "%s\\n" "{\"ready\":true}"' \
    'else' \
    '  printf "%s\\n" "{\"ready\":false}"' \
    'fi' > "$maintainer_bin/runuser"
chmod 0755 "$maintainer_bin/runuser"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "%s\\n" "$*" >> "$SYSTEMCTL_LOG"' \
    'case "$1" in' \
    '  is-active)' \
    '    if [ "${SYSTEMCTL_FAIL_AFTER_START:-0}" = 1 ] && grep -Fq "start teslatlas-hub.service" "$SYSTEMCTL_LOG"; then exit 1; fi' \
    '    [ "${SYSTEMCTL_ACTIVE:-0}" = 1 ]' \
    '    ;;' \
    '  is-enabled) [ "${SYSTEMCTL_ENABLED:-0}" = 1 ] ;;' \
    '  *) exit 0 ;;' \
    'esac' > "$maintainer_bin/systemctl"
chmod 0755 "$maintainer_bin/systemctl"

for script in preinst postinst prerm postrm; do
    sed "s|/run/systemd/system|$systemd_dir|g" "$root/packaging/linux/$script" > "$test_root/$script"
    chmod 0755 "$test_root/$script"
done

printf '%s\n' 'data_dir = "/var/lib/teslatlas-hub"' > "$config_file"

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" SYSTEMCTL_ACTIVE=0 \
    sh "$test_root/postinst" configure ''
require_log 'daemon-reload'
if grep -Fqx 'restart teslatlas-hub.service' "$systemctl_log"; then
    fail 'inactive initial install restarted service'
fi
grep -Fqx '[geocoder]' "$config_file" || fail 'minimal config did not disable geocoder explicitly'
grep -Fqx '[terrain]' "$config_file" || fail 'minimal config did not disable terrain explicitly'

printf '%s\n' \
    'data_dir = "/srv/teslatlas-custom"' \
    '[geocoder]' \
    'provider = "offline"' \
    '[terrain]' \
    'enabled = true' > "$config_file"
: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" SYSTEMCTL_ACTIVE=0 \
    sh "$test_root/postinst" configure ''
[ "$(grep -Ec '^[[:space:]]*enabled[[:space:]]*=' "$config_file")" -eq 2 ] \
    || fail 'existing offline sections did not receive exactly one missing default'
awk '
    /^\[geocoder\]$/ { section = "geocoder"; next }
    /^\[terrain\]$/ { section = "terrain"; next }
    /^\[/ { section = "" }
    section == "geocoder" && /^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false/ { geocoder = 1 }
    section == "terrain" && /^[[:space:]]*enabled[[:space:]]*=[[:space:]]*true/ { terrain = 1 }
    END { exit !(geocoder && terrain) }
' "$config_file" || fail 'offline defaults did not preserve explicit settings'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' new-binary > "$installed_binary"
printf '%s\n' new-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 HUB_READY=1 \
    sh "$test_root/postinst" configure 1.0.0
require_log 'daemon-reload'
require_log 'start teslatlas-hub.service'
grep -Fqx -- "-u teslatlas -- $installed_binary --config $config_file status" \
    "$hub_status_log" || fail 'upgrade did not query Hub readiness as the service user'
[ ! -e "$upgrade_backup" ] || fail 'successful upgrade retained old payload'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' same-schema-unhealthy > "$installed_binary"
printf '%s\n' same-schema-unit > "$installed_unit"
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_READY=0 \
    sh "$test_root/postinst" configure 1.0.0; then
    fail 'same-schema unhealthy Hub was accepted'
fi
grep -Fqx old-binary "$installed_binary" \
    || fail 'same-schema health failure did not restore old binary'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' migrated-binary > "$installed_binary"
printf '%s\n' migrated-unit > "$installed_unit"
rm -f "$schema_upgraded"
printf '%s\n' \
    'data_dir = "/srv/teslatlas-custom"' \
    '[geocoder]' \
    'enabled = false' \
    '[terrain]' \
    'enabled = false' > "$config_file"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_SCHEMA_UPGRADE=1 HUB_READY=1 \
    sh "$test_root/postinst" configure 1.0.0
grep -Fq ' bootstrap' "$hub_status_log" || fail 'schema-changing upgrade did not run bounded bootstrap'
grep -Fqx -- "-u teslatlas -- $installed_binary --config $config_file bootstrap" \
    "$hub_status_log" || fail 'custom data_dir upgrade did not cross the bootstrap boundary'
grep -Fqx migrated-binary "$installed_binary" || fail 'successful schema upgrade restored old binary'
[ ! -e "$upgrade_backup" ] || fail 'successful schema upgrade retained old payload'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' migration-failed-binary > "$installed_binary"
printf '%s\n' migration-failed-unit > "$installed_unit"
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_PREFLIGHT_READY=0 HUB_BOOTSTRAP_READY=0 \
    sh "$test_root/postinst" configure 1.0.0; then
    fail 'failed schema migration was accepted'
fi
grep -Fqx migration-failed-binary "$installed_binary" \
    || fail 'failed schema migration restored incompatible old binary'
[ ! -e "$upgrade_backup" ] || fail 'failed schema migration retained old payload backup'
require_log 'stop teslatlas-hub.service'

: > "$systemctl_log"
mkdir -p "$upgrade_backup"
printf '%s\n' old-binary > "$upgrade_backup/teslatlas-hub"
printf '%s\n' old-unit > "$upgrade_backup/teslatlas-hub.service"
: > "$upgrade_backup/forward-only"
printf '%s\n' '1 1' > "$maintainer_state"
printf '%s\n' forward-only-new > "$installed_binary"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/postrm" failed-upgrade
grep -Fqx forward-only-new "$installed_binary" \
    || fail 'dpkg failed-upgrade restored old payload after migration boundary'
[ ! -e "$upgrade_backup" ] || fail 'dpkg failed-upgrade retained forward-only backup'
[ ! -e "$maintainer_state" ] || fail 'dpkg failed-upgrade retained forward-only state'
require_log 'stop teslatlas-hub.service'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' unhealthy-binary > "$installed_binary"
rm -f "$schema_upgraded"
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_SCHEMA_UPGRADE=1 HUB_READY=0 \
    sh "$test_root/postinst" configure 1.0.0; then
    fail 'active but unready Hub was accepted'
fi
grep -Fqx unhealthy-binary "$installed_binary" \
    || fail 'post-migration health failure restored incompatible old binary'
[ ! -e "$upgrade_backup" ] || fail 'post-migration health failure retained old payload backup'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/prerm" remove
require_log 'is-active --quiet teslatlas-hub.service'
require_log 'is-enabled --quiet teslatlas-hub.service'
require_log 'stop teslatlas-hub.service'
require_log 'disable teslatlas-hub.service'
[ "$(cat "$maintainer_state")" = '1 1' ] || fail 'remove did not preserve active/enabled state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/postinst" abort-remove
require_log 'daemon-reload'
require_log 'enable teslatlas-hub.service'
require_log 'start teslatlas-hub.service'
[ ! -e "$maintainer_state" ] || fail 'successful abort-remove retained rollback state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=0 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/prerm" deconfigure
[ "$(cat "$maintainer_state")" = '0 1' ] || fail 'deconfigure did not preserve inactive/enabled state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/postinst" abort-deconfigure
require_log 'enable teslatlas-hub.service'
require_log 'stop teslatlas-hub.service'
[ ! -e "$maintainer_state" ] || fail 'successful abort-deconfigure retained rollback state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=0 \
    sh "$test_root/prerm" remove
[ "$(cat "$maintainer_state")" = '1 0' ] || fail 'remove did not preserve active/disabled state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/postrm" abort-upgrade
require_log 'disable teslatlas-hub.service'
require_log 'start teslatlas-hub.service'
[ ! -e "$maintainer_state" ] || fail 'successful abort-upgrade retained rollback state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/postinst" abort-upgrade
require_log 'daemon-reload'

for action in abort-install abort-upgrade failed-upgrade; do
    : > "$systemctl_log"
    PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
        sh "$test_root/postrm" "$action"
    require_log 'daemon-reload'
done

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/prerm" upgrade
[ ! -s "$systemctl_log" ] || fail 'upgrade prerm changed service state'

for action in remove purge; do
    printf '%s\n' '1 1' > "$maintainer_state"
    : > "$systemctl_log"
    PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
        sh "$test_root/postrm" "$action"
    require_log 'daemon-reload'
    if grep -Fqx 'disable teslatlas-hub.service' "$systemctl_log"; then
        fail "$action tried to disable an already removed unit"
    fi
    [ ! -e "$maintainer_state" ] || fail "$action retained rollback state"
done

printf '%s\n' \
    '#!/bin/sh' \
    'case "$1" in' \
    '  -h)' \
    '    case "${FAKE_ELF_MODE:-good_amd64}" in' \
    "      good_amd64|bad_endian|bad_abi|bad_interpreter|dynamic_without_interpreter|relocatable|shared_object) machine='Advanced Micro Devices X86-64' ;;" \
    "      good_arm64|good_arm64_static) machine='AArch64' ;;" \
    '    esac' \
    '    data="2'"'"'s complement, little endian"' \
    '    [ "${FAKE_ELF_MODE:-}" = bad_endian ] && data="2'"'"'s complement, big endian"' \
    '    abi="UNIX - System V"' \
    '    [ "${FAKE_ELF_MODE:-}" = bad_abi ] && abi="UNIX - Solaris"' \
    '    type="EXEC (Executable file)"' \
    '    [ "${FAKE_ELF_MODE:-}" = good_amd64 ] && type="DYN (Position-Independent Executable file)"' \
    '    [ "${FAKE_ELF_MODE:-}" = relocatable ] && type="REL (Relocatable file)"' \
    '    [ "${FAKE_ELF_MODE:-}" = shared_object ] && type="DYN (Shared object file)"' \
    '    printf "  Class: ELF64\\n  Data: %s\\n  OS/ABI: %s\\n  Type: %s\\n  Machine: %s\\n" "$data" "$abi" "$type" "$machine"' \
    '    ;;' \
    '  -l)' \
    '    case "${FAKE_ELF_MODE:-good_amd64}" in' \
    '      good_amd64) printf "Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]\\n" ;;' \
    '      good_arm64) printf "Requesting program interpreter: /lib/ld-linux-aarch64.so.1]\\n" ;;' \
    '      bad_endian|bad_abi) printf "Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]\\n" ;;' \
    '      bad_interpreter) printf "Requesting program interpreter: /bad/loader]\\n" ;;' \
    '    esac' \
    '    ;;' \
    '  -d)' \
    '    [ "${FAKE_ELF_MODE:-}" = dynamic_without_interpreter ] && printf " 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]\\n"' \
    '    ;;' \
    'esac' > "$elf_bin/readelf"
chmod 0755 "$elf_bin/readelf"
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'while [ "$#" -gt 2 ]; do' \
    '  case "$1" in' \
    '    -D) shift ;;' \
    '    -m) shift 2 ;;' \
    '    *) exit 64 ;;' \
    '  esac' \
    'done' \
    'mkdir -p "$(dirname -- "$2")"' \
    'cp "$1" "$2"' > "$elf_bin/install"
chmod 0755 "$elf_bin/install"
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '[ "$1" = -O ]' \
    '[ "$2" = -e ]' \
    '[ -f "$3" ]' \
    '[ -f debian/control ]' \
    'case "${FAKE_SHLIBDEPS_MODE:-good}" in' \
    '  good) printf "%s\\n" "shlibs:Depends=libc6 (>= 2.38), libgcc-s1 (>= 3.0)" ;;' \
    '  static) printf "%s\\n" "shlibs:Depends=" ;;' \
    '  missing) printf "%s\\n" "misc:Depends=ignored" ;;' \
    '  duplicate) printf "%s\\n" "shlibs:Depends=libc6" "shlibs:Depends=libgcc-s1" ;;' \
    '  fail) exit 1 ;;' \
    '  *) exit 64 ;;' \
    'esac' > "$elf_bin/dpkg-shlibdeps"
chmod 0755 "$elf_bin/dpkg-shlibdeps"
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '[ "$1" = --root-owner-group ] && shift' \
    '[ "$1" = --build ] && shift' \
    '[ -f "$1/DEBIAN/preinst" ]' \
    '[ -f "$1/DEBIAN/postrm" ]' \
    'cmp "$EXPECTED_LICENSE" "$1/usr/share/doc/teslatlas-hub/copyright"' \
    'cmp "$EXPECTED_NOTICE" "$1/usr/share/doc/teslatlas-hub/NOTICE"' \
    'cmp "$EXPECTED_THIRD_PARTY_NOTICES" "$1/usr/share/doc/teslatlas-hub/THIRD_PARTY_NOTICES.md"' \
    'cmp "$EXPECTED_PROVENANCE" "$1/usr/share/doc/teslatlas-hub/PROVENANCE.md"' \
    'cp "$1/DEBIAN/control" "$CAPTURED_CONTROL"' \
    ': > "$2"' > "$elf_bin/dpkg-deb"
chmod 0755 "$elf_bin/dpkg-deb"
: > "$test_root/fake-binary"

run_package() {
    (
        cd "$test_root"
        FAKE_ELF_MODE=$1 FAKE_SHLIBDEPS_MODE=${3:-good} \
            CAPTURED_CONTROL="$captured_control" EXPECTED_LICENSE="$root/LICENSE" \
            EXPECTED_NOTICE="$root/NOTICE" EXPECTED_THIRD_PARTY_NOTICES="$root/THIRD_PARTY_NOTICES.md" \
            EXPECTED_PROVENANCE="$root/PROVENANCE.md" \
            PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
            sh "$root/scripts/build-deb.sh" --binary fake-binary \
            --version "${4:-1.0.0}" --architecture "$2" --output "$test_root/output.deb"
    ) > "$test_root/package-output" 2>&1
}

ln -s fake-binary "$test_root/fake-binary-link"
if (
    cd "$test_root"
    PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
        sh "$root/scripts/build-deb.sh" --binary fake-binary-link \
        --version 1.0.0 --architecture amd64 --output "$test_root/symlink.deb"
) >"$test_root/symlink-output" 2>&1; then
    fail 'symlinked binary accepted'
fi

run_package good_amd64 amd64 || fail 'valid amd64 ELF rejected'
grep -Fqx 'Version: 1.0.0-1' "$captured_control" \
    || fail 'stable semver was not mapped to a Debian revision'
grep -Fqx 'Depends: adduser, ca-certificates, systemd, libc6 (>= 2.38), libgcc-s1 (>= 3.0)' \
    "$captured_control" || fail 'amd64 shared-library dependencies missing from control'
grep -Fqx 'Description: Self-hosted multi-car Tesla telemetry hub' "$captured_control" \
    || fail 'package description does not describe multi-car support'
run_package good_arm64 arm64 || fail 'valid dynamic arm64 ELF rejected'
grep -Fqx 'Depends: adduser, ca-certificates, systemd, libc6 (>= 2.38), libgcc-s1 (>= 3.0)' \
    "$captured_control" || fail 'arm64 shared-library dependencies missing from control'
run_package good_arm64_static arm64 || fail 'valid static arm64 ELF rejected'
grep -Fqx 'Depends: adduser, ca-certificates, systemd' "$captured_control" \
    || fail 'static arm64 package gained shared-library dependencies'
run_package good_amd64 amd64 good 1.0.0-alpha.1 || fail 'valid prerelease rejected'
grep -Fqx 'Version: 1.0.0~alpha.1-1' "$captured_control" \
    || fail 'prerelease does not sort before the stable Debian package'
for mode in bad_endian bad_abi bad_interpreter dynamic_without_interpreter relocatable shared_object; do
    if run_package "$mode" amd64; then
        fail "invalid ELF accepted: $mode"
    fi
done
for mode in missing duplicate fail; do
    if run_package good_amd64 amd64 "$mode"; then
        fail "invalid dpkg-shlibdeps result accepted: $mode"
    fi
done

echo 'test-linux-packaging: PASS'
