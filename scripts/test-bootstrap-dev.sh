#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-bootstrap-dev-test.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

core_prefix="$temporary/core"
sh "$root/scripts/bootstrap-dev.sh" \
    --prefix "$core_prefix" \
    --components none \
    --dry-run | python3 -c '
import json
import sys

assert json.load(sys.stdin) == {
    "schema": "teslatlas.bootstrap-dev-plan/v1",
    "prefix": sys.argv[1],
    "components": ["hub-core"],
    "companion_components": [],
}
' "$core_prefix"

if sh "$root/scripts/bootstrap-dev.sh" --prefix "$temporary/rejected" --components deferred-product --dry-run >/dev/null 2>&1; then
    echo "deferred components must be rejected" >&2
    exit 1
fi

fake_bin="$temporary/bin"
mkdir "$fake_bin"
printf '%s\n' \
    '#!/bin/sh' \
    '[ "${1:-}" = --version ] || exit 99' \
    "printf '%s\\n' 'teslatlas-hub 2026.36.2'" \
    > "$fake_bin/teslatlas-hub"
chmod 0755 "$fake_bin/teslatlas-hub"

PATH="$fake_bin:$PATH" sh "$root/scripts/bootstrap-dev.sh" \
    --prefix "$core_prefix" \
    --components none | python3 -c '
import json
import sys

assert json.load(sys.stdin) == {
    "schema": "teslatlas.bootstrap-dev-result/v1",
    "status": "hub-core-ready",
    "hub_version": "2026.36.2",
    "hub_url": "https://127.0.0.1:8443",
}
'
