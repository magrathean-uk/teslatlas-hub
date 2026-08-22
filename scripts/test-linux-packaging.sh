#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-package-test.XXXXXX")
trap 'find "$test_root" -depth -delete' EXIT HUP INT TERM
maintainer_bin="$test_root/maintainer-bin"
elf_bin="$test_root/elf-bin"
systemd_dir="$test_root/systemd"
systemctl_log="$test_root/systemctl.log"
maintainer_state="$test_root/maintainer.state"
captured_control="$test_root/control"
mkdir -p "$maintainer_bin" "$elf_bin" "$systemd_dir"
export TESLATLAS_HUB_MAINTAINER_STATE_FILE="$maintainer_state"

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

for utility in getent adduser install chown chmod; do
    printf '%s\n' '#!/bin/sh' 'exit 0' > "$maintainer_bin/$utility"
    chmod 0755 "$maintainer_bin/$utility"
done
printf '%s\n' \
    '#!/bin/sh' \
    'printf "%s\\n" "$*" >> "$SYSTEMCTL_LOG"' \
    'case "$1" in' \
    '  is-active) [ "${SYSTEMCTL_ACTIVE:-0}" = 1 ] ;;' \
    '  is-enabled) [ "${SYSTEMCTL_ENABLED:-0}" = 1 ] ;;' \
    '  *) exit 0 ;;' \
    'esac' > "$maintainer_bin/systemctl"
chmod 0755 "$maintainer_bin/systemctl"

for script in postinst prerm postrm; do
    sed "s|/run/systemd/system|$systemd_dir|g" "$root/packaging/linux/$script" > "$test_root/$script"
    chmod 0755 "$test_root/$script"
done

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" SYSTEMCTL_ACTIVE=0 \
    sh "$test_root/postinst" configure ''
require_log 'daemon-reload'
if grep -Fqx 'restart teslatlas-hub.service' "$systemctl_log"; then
    fail 'inactive initial install restarted service'
fi

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" SYSTEMCTL_ACTIVE=1 \
    sh "$test_root/postinst" configure 1.0.0
require_log 'daemon-reload'
require_log 'is-active --quiet teslatlas-hub.service'
require_log 'restart teslatlas-hub.service'

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
    "      good_amd64|bad_endian|bad_abi|bad_interpreter|dynamic_without_interpreter) machine='Advanced Micro Devices X86-64' ;;" \
    "      good_arm64|good_arm64_static) machine='AArch64' ;;" \
    '    esac' \
    '    data="2'"'"'s complement, little endian"' \
    '    [ "${FAKE_ELF_MODE:-}" = bad_endian ] && data="2'"'"'s complement, big endian"' \
    '    abi="UNIX - System V"' \
    '    [ "${FAKE_ELF_MODE:-}" = bad_abi ] && abi="UNIX - Solaris"' \
    '    printf "  Class: ELF64\\n  Data: %s\\n  OS/ABI: %s\\n  Machine: %s\\n" "$data" "$abi" "$machine"' \
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
    '[ -f "$1/DEBIAN/postrm" ]' \
    'cp "$1/DEBIAN/control" "$CAPTURED_CONTROL"' \
    ': > "$2"' > "$elf_bin/dpkg-deb"
chmod 0755 "$elf_bin/dpkg-deb"
: > "$test_root/fake-binary"

run_package() {
    (
        cd "$test_root"
        FAKE_ELF_MODE=$1 FAKE_SHLIBDEPS_MODE=${3:-good} \
            CAPTURED_CONTROL="$captured_control" PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
            sh "$root/scripts/build-deb.sh" --binary fake-binary \
            --version 1.0.0 --architecture "$2" --output "$test_root/output.deb"
    ) > "$test_root/package-output" 2>&1
}

run_package good_amd64 amd64 || fail 'valid amd64 ELF rejected'
grep -Fqx 'Depends: adduser, ca-certificates, systemd, libc6 (>= 2.38), libgcc-s1 (>= 3.0)' \
    "$captured_control" || fail 'amd64 shared-library dependencies missing from control'
run_package good_arm64 arm64 || fail 'valid dynamic arm64 ELF rejected'
grep -Fqx 'Depends: adduser, ca-certificates, systemd, libc6 (>= 2.38), libgcc-s1 (>= 3.0)' \
    "$captured_control" || fail 'arm64 shared-library dependencies missing from control'
run_package good_arm64_static arm64 || fail 'valid static arm64 ELF rejected'
grep -Fqx 'Depends: adduser, ca-certificates, systemd' "$captured_control" \
    || fail 'static arm64 package gained shared-library dependencies'
for mode in bad_endian bad_abi bad_interpreter dynamic_without_interpreter; do
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
