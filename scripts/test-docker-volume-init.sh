#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

if [ "$(uname -s)" = Darwin ] &&
    { ! command -v gstat >/dev/null 2>&1 || ! command -v gln >/dev/null 2>&1; }; then
    printf '%s\n' 'GNU coreutils (gstat, gln) required on macOS: brew install coreutils' >&2
    exit 69
fi

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/tesatlas-volume-init-test.XXXXXX")
trap 'find "$temporary" -depth -delete' EXIT HUP INT TERM
volume_probe="$temporary/volume"
protected_target="$temporary/protected-target"
protected_hardlink="$temporary/protected-hardlink"
race_target="$temporary/race-target"
mkdir "$volume_probe"

# Execute the shipped algorithm without privilege using its explicit test-only
# path. Only its exact empty regular sentinel is idempotent.
TESATLAS_HUB_VOLUME_INIT_TEST_ONLY=1 "$root/packaging/docker/initialize-volume.sh" \
    --test-only "$volume_probe" "$(id -u)" "$(id -g)"
TESATLAS_HUB_VOLUME_INIT_TEST_ONLY=1 "$root/packaging/docker/initialize-volume.sh" \
    --test-only "$volume_probe" "$(id -u)" "$(id -g)"
python3 - "$volume_probe/.tesatlas-volume-initialized" "$(id -u)" "$(id -g)" <<'PY'
import os
import stat
import sys

status = os.lstat(sys.argv[1])
assert stat.S_ISREG(status.st_mode)
assert not stat.S_ISLNK(status.st_mode)
assert status.st_uid == int(sys.argv[2])
assert status.st_gid == int(sys.argv[3])
assert stat.S_IMODE(status.st_mode) == 0o600
assert status.st_size == 0
assert status.st_nlink == 1
PY

# A symlink must fail without following it or changing its target.
rm "$volume_probe/.tesatlas-volume-initialized"
printf '%s\n' 'must-not-change' > "$protected_target"
ln -s "$protected_target" "$volume_probe/.tesatlas-volume-initialized"
if TESATLAS_HUB_VOLUME_INIT_TEST_ONLY=1 "$root/packaging/docker/initialize-volume.sh" \
    --test-only "$volume_probe" "$(id -u)" "$(id -g)" >/dev/null 2>&1; then
    printf '%s\n' 'initializer accepted a sentinel symlink' >&2
    exit 1
fi
test "$(cat "$protected_target")" = 'must-not-change'
test -L "$volume_probe/.tesatlas-volume-initialized"

# A metadata-compatible hard link is not the unique sentinel and must fail
# without changing the linked target.
rm "$volume_probe/.tesatlas-volume-initialized"
: > "$protected_hardlink"
chmod 0600 "$protected_hardlink"
ln "$protected_hardlink" "$volume_probe/.tesatlas-volume-initialized"
if TESATLAS_HUB_VOLUME_INIT_TEST_ONLY=1 "$root/packaging/docker/initialize-volume.sh" \
    --test-only "$volume_probe" "$(id -u)" "$(id -g)" >/dev/null 2>&1; then
    printf '%s\n' 'initializer accepted a multiply linked regular sentinel' >&2
    exit 1
fi
test ! -s "$protected_hardlink"
test -f "$volume_probe/.tesatlas-volume-initialized"

# A wrong regular file must also fail without truncation or normalization.
rm "$volume_probe/.tesatlas-volume-initialized"
printf '%s\n' 'wrong-regular-entry' > "$volume_probe/.tesatlas-volume-initialized"
if TESATLAS_HUB_VOLUME_INIT_TEST_ONLY=1 "$root/packaging/docker/initialize-volume.sh" \
    --test-only "$volume_probe" "$(id -u)" "$(id -g)" >/dev/null 2>&1; then
    printf '%s\n' 'initializer accepted a non-empty regular sentinel' >&2
    exit 1
fi
test "$(cat "$volume_probe/.tesatlas-volume-initialized")" = 'wrong-regular-entry'

rm "$volume_probe/.tesatlas-volume-initialized"
mkdir "$volume_probe/.tesatlas-volume-initialized"
if TESATLAS_HUB_VOLUME_INIT_TEST_ONLY=1 "$root/packaging/docker/initialize-volume.sh" \
    --test-only "$volume_probe" "$(id -u)" "$(id -g)" >/dev/null 2>&1; then
    printf '%s\n' 'initializer accepted a sentinel directory' >&2
    exit 1
fi
test -d "$volume_probe/.tesatlas-volume-initialized"

# Deterministically create a destination symlink after the temporary inode is
# fully prepared but immediately before atomic publication. The hard link must
# fail without following or replacing the raced entry.
rm -r "$volume_probe/.tesatlas-volume-initialized"
printf '%s\n' 'race-target-must-not-change' > "$race_target"
if TESATLAS_HUB_VOLUME_INIT_TEST_ONLY=1 \
    TESATLAS_HUB_VOLUME_INIT_TEST_RACE_TARGET="$race_target" \
    "$root/packaging/docker/initialize-volume.sh" \
    --test-only "$volume_probe" "$(id -u)" "$(id -g)" >/dev/null 2>&1; then
    printf '%s\n' 'initializer replaced a sentinel created at publication' >&2
    exit 1
fi
test "$(cat "$race_target")" = 'race-target-must-not-change'
test -L "$volume_probe/.tesatlas-volume-initialized"

printf '%s\n' 'docker volume initializer adversarial checks passed'
