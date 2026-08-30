#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
VERIFIER="$ROOT/scripts/verify-repository-layout.py"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-layout-test.XXXXXX")
cleanup() {
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

make_fixture() {
    destination=$1
    mkdir -p "$destination/.github" "$destination/docs/guides"
    git -C "$destination" init -q
    for name in README SECURITY SUPPORT CONTRIBUTING CODE_OF_CONDUCT; do
        printf '# %s\n' "$name" >"$destination/.github/$name.md"
    done
    printf '%s\n' '# Documentation' '[Guide](guides/operator-guide.md)' \
        >"$destination/docs/index.md"
    printf '%s\n' '# Operator guide' '[Security](../../.github/SECURITY.md)' \
        >"$destination/docs/guides/operator-guide.md"
    git -C "$destination" add .
}

make_fixture "$TMP/valid"
python3 "$VERIFIER" --repo "$TMP/valid" >/dev/null

cp -R "$TMP/valid" "$TMP/root-markdown"
printf '%s\n' '# forbidden' >"$TMP/root-markdown/README.md"
git -C "$TMP/root-markdown" add README.md
if python3 "$VERIFIER" --repo "$TMP/root-markdown" >/dev/null 2>&1; then
    echo 'repository layout test: root Markdown was accepted' >&2
    exit 1
fi

cp -R "$TMP/valid" "$TMP/bad-name"
printf '%s\n' '# bad name' >"$TMP/bad-name/docs/guides/Bad_Name.md"
git -C "$TMP/bad-name" add docs/guides/Bad_Name.md
if python3 "$VERIFIER" --repo "$TMP/bad-name" >/dev/null 2>&1; then
    echo 'repository layout test: nonconforming documentation name was accepted' >&2
    exit 1
fi

cp -R "$TMP/valid" "$TMP/broken-link"
printf '%s\n' '# Documentation' '[Missing](guides/missing.md)' \
    >"$TMP/broken-link/docs/index.md"
git -C "$TMP/broken-link" add docs/index.md
if python3 "$VERIFIER" --repo "$TMP/broken-link" >/dev/null 2>&1; then
    echo 'repository layout test: broken local link was accepted' >&2
    exit 1
fi

echo 'repository layout tests passed'
