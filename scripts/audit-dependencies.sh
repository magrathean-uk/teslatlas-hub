#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only

set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cargo=${CARGO:-cargo}
advisory=RUSTSEC-2026-0235

cd "$root"

python3 "$root/scripts/verify-provenance.py" --repo "$root"

if grep -A2 -F 'name = "rkyv"' Cargo.lock | grep -Fq 'version = "0.7.46"'; then
    reverse=$($cargo tree --locked --target all --edges normal,build -i rkyv@0.7.46 2>/dev/null)
    [ -z "$reverse" ] || {
        printf '%s\n' "audit-dependencies: $advisory is reachable from the release graph" >&2
        exit 1
    }
    $cargo audit --no-fetch --stale --ignore "$advisory"
    printf '%s\n' "dependency audit passed; $advisory is lockfile-only and unreachable"
else
    $cargo audit --no-fetch --stale
    printf '%s\n' 'dependency audit passed'
fi
