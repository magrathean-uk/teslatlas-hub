#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-bootstrap-dev-test.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

assert_plan() {
    expected_components=$1
    expected_companions=$2
    prefix=$3
    python3 -c '
import json
import sys

plan = json.load(sys.stdin)
assert plan["schema"] == "teslatlas.bootstrap-dev-plan/v1", plan
assert plan["prefix"] == sys.argv[1], plan
assert plan["components"] == sys.argv[2].split(","), plan
assert plan["companion_components"] == (
    [] if sys.argv[3] == "" else sys.argv[3].split(",")
), plan
' "$prefix" "$expected_components" "$expected_companions"
}

viewer_prefix="$temporary/viewer"
sh "$root/scripts/bootstrap-dev.sh" \
    --prefix "$viewer_prefix" \
    --components viewer \
    --dry-run | assert_plan 'hub-core,sdk-typescript,viewer' 'sdk-typescript,viewer' "$viewer_prefix"

core_prefix="$temporary/core"
sh "$root/scripts/bootstrap-dev.sh" \
    --prefix "$core_prefix" \
    --components none \
    --dry-run | assert_plan 'hub-core' '' "$core_prefix"

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

result = json.load(sys.stdin)
assert result == {
    "schema": "teslatlas.bootstrap-dev-result/v1",
    "status": "hub-core-ready",
    "hub_version": "2026.36.2",
    "hub_url": "https://127.0.0.1:8443",
    "viewer_url": None,
}, result
'
