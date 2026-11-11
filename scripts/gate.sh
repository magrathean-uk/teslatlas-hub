#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Local Hub gate. Usage: scripts/gate.sh [--dry-run] quick|full
# Local only: never pushes, tags or calls GitHub.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
dry=0
mode=
for arg in "$@"; do
    case $arg in
        --dry-run) dry=1 ;;
        quick | full) mode=$arg ;;
        *)
            printf '%s\n' "usage: scripts/gate.sh [--dry-run] quick|full" >&2
            exit 2
            ;;
    esac
done
[ -n "$mode" ] || {
    printf '%s\n' "usage: scripts/gate.sh [--dry-run] quick|full" >&2
    exit 2
}

if [ "$mode" = full ] && [ "$dry" -eq 0 ]; then
    hour=$(date -u +%H)
    case $hour in
        23 | 00 | 01 | 02 | 03 | 04 | 05 | 06) ;;
        *)
            printf '%s\n' "gate: full runs only 23:00 to 07:00 GMT (now ${hour}h GMT)" >&2
            exit 2
            ;;
    esac
fi

# By-day limits: one build at 3 jobs and 3 test threads, nice 12.
export CARGO_BUILD_JOBS=3
export RUST_TEST_THREADS=3
export NICE_LEVEL=12

checks=
add() { checks="$checks$1|$2|$3
"; }

# name|directory (relative to root)|command
add layout . "python3 scripts/verify-repository-layout.py"
add provenance . "python3 scripts/verify-provenance.py"
add name-policy . "python3 scripts/verify-name-policy.py --strict"
add own-code . "python3 scripts/verify-own-code.py"
add gitleaks . "gitleaks git --no-banner --redact ."
add cargo-check . "cargo check --locked"
add clippy . "cargo clippy --locked --all-targets -- -D warnings"
add compute-tests . "cargo test --locked -p teslatlas-compute"
add protocol-check protocol "tools/check"
add sdk-protocol sdk-typescript "npm run protocol:check"
add sdk-typecheck sdk-typescript "npm run typecheck"
add sdk-tests sdk-typescript "npm test"
if [ "$mode" = full ]; then
    add cargo-test . "cargo test --locked"
    add audit . "scripts/audit-dependencies.sh"
fi

if [ "$dry" -eq 1 ]; then
    printf '%s' "$checks" | while IFS='|' read -r name dir cmd; do
        [ -n "$name" ] || continue
        printf '%s\n' "would run $name: (cd $dir && nice -n 12 $cmd)"
    done
    exit 0
fi

# Workspace that owns the heavy-build lock and the toolchain environment.
workspace=
for base in "$root/.." "${TESLATLAS_WORKSPACE:-$HOME/dev/source/teslatlas-service}"; do
    if [ -x "$base/scripts/dev/with-heavy-build-lock.sh" ] && [ -r "$base/scripts/dev/env.sh" ]; then
        workspace=$(CDPATH='' cd -- "$base" && pwd)
        break
    fi
done
[ -n "$workspace" ] || {
    printf '%s\n' "gate: workspace scripts/dev (heavy-build lock, env.sh) not found" >&2
    exit 2
}

# Heavy-build lock: re-run this script under the workspace lock so no other
# gate or workspace build overlaps (non-blocking; exit 75 when held).
if [ -z "${HUB_GATE_LOCKED:-}" ]; then
    HUB_GATE_LOCKED=1 exec "$workspace/scripts/dev/with-heavy-build-lock.sh" "$0" "$@"
fi

# Same environment as scripts/dev/run.sh: pinned Rust toolchain, rustup proxies
# first on PATH, no incremental builds, internal-mode debug settings. Checks
# still run in this checkout.
. "$workspace/scripts/dev/env.sh" || {
    printf '%s\n' "gate: could not load $workspace/scripts/dev/env.sh" >&2
    exit 2
}

# SDK checks use the pinned Node/npm copy named by sdk-typescript/.node-version.
node_version=$(cat "$root/sdk-typescript/.node-version" 2>/dev/null)
case $node_version in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *)
        printf '%s\n' "gate: invalid sdk-typescript/.node-version" >&2
        exit 2
        ;;
esac
node_bin="${DEVMACHINE:-${DEV:-$HOME/dev}/machine}/toolchains/node-pinned/v$node_version/bin"
if [ ! -x "$node_bin/node" ] || [ ! -x "$node_bin/npm" ]; then
    printf '%s\n' "gate: pinned Node toolchain unavailable at $node_bin" >&2
    exit 2
fi

load=$(uptime | sed 's/.*load averages*: *//' | awk '{print int($1)}')
if [ "${load:-0}" -gt 100 ]; then
    printf '%s\n' "gate: 1-minute load average $load is above 100" >&2
    exit 2
fi

fail=0
results=$(mktemp "${TMPDIR:-/tmp}/hub-gate.XXXXXX") || exit 2
trap 'rm -f "$results"' EXIT HUP INT TERM
printf '%s' "$checks" >"$results"
while IFS='|' read -r name dir cmd <&3; do
    [ -n "$name" ] || continue
    (
        cd "$root/$dir" || exit 2
        case $dir in sdk-typescript) PATH="$node_bin:$PATH" ;; esac
        eval "nice -n 12 $cmd"
    ) >/dev/null 2>"${results}.err"
    code=$?
    printf '%s\n' "$name: exit $code"
    if [ "$code" -ne 0 ]; then
        fail=1
        tail -n 5 "${results}.err" >&2
    fi
done 3<"$results"
rm -f "${results}.err"
exit "$fail"
