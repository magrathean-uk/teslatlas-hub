#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only

set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
HELPER="$ROOT/scripts/go_toolchain.py"
APP_BUILD="$ROOT/scripts/build-macos-app.sh"
PROXY_BUILD="$ROOT/scripts/build-tesla-command-proxy.sh"
FLEET_BUILD="$ROOT/scripts/build-fleet-telemetry-bridge.sh"
GO_EVIDENCE="$ROOT/scripts/go-proxy-evidence.py"
FLEET_EVIDENCE="$ROOT/scripts/fleet-telemetry-evidence.py"
TMP=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/teslatlas-go-selection.XXXXXX")

cleanup() {
    /bin/chmod -R u+w "$TMP" 2>/dev/null || true
    /usr/bin/find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

fail() {
    /usr/bin/printf '%s\n' "test-go-toolchain-selection: $*" >&2
    exit 1
}

make_fake_go() {
    fake_path=$1
    fake_version=$2
    fake_log=$3
    fake_cache=$4
    /bin/mkdir -p "$(/usr/bin/dirname "$fake_path")"
    /usr/bin/sed \
        -e "s|@VERSION@|$fake_version|g" \
        -e "s|@LOG@|$fake_log|g" \
        -e "s|@CACHE@|$fake_cache|g" \
        "$TMP/fake-go.in" >"$fake_path"
    /bin/chmod 0700 "$fake_path"
}

/usr/bin/printf '%s\n' \
    '#!/bin/sh' \
    'printf '\''%s\n'\'' "$*" >>"@LOG@"' \
    'if [ "${1-}" = env ] && [ "${2-}" = GOVERSION ]; then' \
    '    printf '\''%s\n'\'' @VERSION@' \
    'elif [ "${1-}" = env ] && [ "${2-}" = -json ]; then' \
    '    printf '\''%s\n'\'' '\''{"GOVERSION":"@VERSION@","GOENV":"","GOWORK":"off","GOTOOLCHAIN":"local","GOFLAGS":"","GOHOSTOS":"darwin","GOHOSTARCH":"arm64"}'\''' \
    'elif [ "${1-}" = env ] && [ "${2-}" = GOMODCACHE ]; then' \
    '    printf '\''%s\n'\'' "@CACHE@"' \
    'else' \
    '    exit 73' \
    'fi' >"$TMP/fake-go.in"

selected_go="$TMP/selected toolchain/bin/go"
selected_log="$TMP/selected.log"
ambient_go="$TMP/ambient/bin/go"
ambient_log="$TMP/ambient.log"
module_cache="$TMP/module cache"
/bin/mkdir -p "$module_cache"
make_fake_go "$selected_go" go1.27.0 "$selected_log" "$module_cache"
make_fake_go "$ambient_go" go1.27.1 "$ambient_log" "$module_cache"

selected=$(
    PATH="$(/usr/bin/dirname "$ambient_go"):/usr/bin:/bin" \
        TESLATLAS_GO="$selected_go" \
        /usr/bin/python3 "$HELPER" --expected-version go1.27.0
) || fail "absolute override was rejected"
[ "$selected" = "$selected_go" ] || fail "override path was not preserved"
[ -s "$selected_log" ] || fail "selected Go was not executed"
[ ! -e "$ambient_log" ] || fail "ambient Go was executed despite the override"

: >"$selected_log"
parent_selected=$(
    PATH="$(/usr/bin/dirname "$ambient_go"):/usr/bin:/bin" \
        TESLATLAS_GO="$selected_go" \
        "$APP_BUILD" --check-go-toolchain-chain
) || fail "macOS parent rejected the explicit Go toolchain"
expected_parent_selection=$(/usr/bin/printf '%s\n' \
    "parent=$selected_go" \
    "tesla-command-proxy=$selected_go" \
    "fleet-telemetry=$selected_go" \
    "go-proxy-evidence=$selected_go" \
    "fleet-telemetry-evidence=$selected_go")
[ "$parent_selected" = "$expected_parent_selection" ] \
    || fail "macOS parent did not propagate one selection to all downstream probes"
[ "$(/usr/bin/wc -l <"$selected_log" | /usr/bin/tr -d '[:space:]')" = 5 ] \
    || fail "parent chain did not validate the selected Go at all five boundaries"
[ ! -e "$ambient_log" ] || fail "macOS parent fell back to ambient Go"

: >"$selected_log"
if TESLATLAS_GO="$selected_go" "$PROXY_BUILD" \
    --target linux-amd64 --output "$TMP/proxy" >"$TMP/proxy.out" 2>&1; then
    fail "proxy fixture unexpectedly completed"
fi
/usr/bin/grep -Fq 'cannot download Tesla vehicle-command module' "$TMP/proxy.out" \
    || fail "proxy builder did not reach its selected-Go boundary"
/usr/bin/grep -Fq 'mod download' "$selected_log" \
    || fail "proxy builder did not use the explicit Go"

: >"$selected_log"
if TESLATLAS_GO="$selected_go" "$FLEET_BUILD" \
    --target linux-amd64 --output "$TMP/missing/fleet" >"$TMP/fleet.out" 2>&1; then
    fail "Fleet fixture unexpectedly completed"
fi
/usr/bin/grep -Fq 'output directory does not exist' "$TMP/fleet.out" \
    || fail "Fleet builder did not reach its post-toolchain boundary"
[ -s "$selected_log" ] || fail "Fleet builder did not validate the explicit Go"

: >"$selected_log"
PATH="$(/usr/bin/dirname "$ambient_go"):/usr/bin:/bin" \
    TESLATLAS_GO="$selected_go" /usr/bin/python3 - \
    "$ROOT/scripts" "$GO_EVIDENCE" "$selected_go" <<'PY'
import importlib.util
import pathlib
import sys

scripts, source, expected = sys.argv[1:]
sys.path.insert(0, scripts)
spec = importlib.util.spec_from_file_location("go_proxy_evidence", source)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)
selected, _, values = module.strict_go_environment()
assert selected == expected
assert values["GOVERSION"] == "go1.27.0"
PY
[ -s "$selected_log" ] || fail "Go proxy evidence did not use the explicit Go"
[ ! -e "$ambient_log" ] || fail "Go proxy evidence fell back to ambient Go"

: >"$selected_log"
PATH="$(/usr/bin/dirname "$ambient_go"):/usr/bin:/bin" \
    TESLATLAS_GO="$selected_go" /usr/bin/python3 - \
    "$ROOT/scripts" "$FLEET_EVIDENCE" "$module_cache" <<'PY'
import importlib.util
import pathlib
import sys

scripts, source, expected = sys.argv[1:]
sys.path.insert(0, scripts)
spec = importlib.util.spec_from_file_location("fleet_telemetry_evidence", source)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)
assert module.default_module_cache() == pathlib.Path(expected)
PY
[ -s "$selected_log" ] || fail "Fleet evidence did not use the explicit Go"
[ ! -e "$ambient_log" ] || fail "Fleet evidence fell back to ambient Go"

evidence_wrong_log="$TMP/evidence-wrong.log"
evidence_wrong_go="$TMP/evidence-wrong/bin/go"
evidence_fallback_log="$TMP/evidence-fallback.log"
evidence_fallback_go="$TMP/evidence-fallback/bin/go"
make_fake_go "$evidence_wrong_go" go1.27.1 "$evidence_wrong_log" "$module_cache"
make_fake_go "$evidence_fallback_go" go1.27.0 "$evidence_fallback_log" "$module_cache"
if PATH="$(/usr/bin/dirname "$evidence_fallback_go"):/usr/bin:/bin" \
    GOMODCACHE="$module_cache" TESLATLAS_GO="$evidence_wrong_go" \
    /usr/bin/python3 "$FLEET_EVIDENCE" --repo "$ROOT" \
    --receiver-binary "$TMP/missing-receiver" \
    --output-dir "$TMP/inherited-cache-output" >"$TMP/inherited-cache.out" 2>&1; then
    fail "Fleet evidence accepted a wrong override with inherited GOMODCACHE"
fi
/usr/bin/grep -Fq 'go1.27.0 is required exactly: go1.27.1' \
    "$TMP/inherited-cache.out" \
    || fail "inherited GOMODCACHE bypassed explicit Go validation"
[ -s "$evidence_wrong_log" ] || fail "inherited-cache override was not checked"
[ ! -e "$evidence_fallback_log" ] \
    || fail "inherited-cache override silently fell back to ambient Go"

/bin/rm -f "$evidence_fallback_log"
missing_override="$TMP/missing explicit/go"
if PATH="$(/usr/bin/dirname "$evidence_fallback_go"):/usr/bin:/bin" \
    TESLATLAS_GO="$missing_override" \
    /usr/bin/python3 "$FLEET_EVIDENCE" --repo "$ROOT" \
    --receiver-binary "$TMP/missing-receiver" --module-cache "$module_cache" \
    --output-dir "$TMP/explicit-cache-output" >"$TMP/explicit-cache.out" 2>&1; then
    fail "Fleet evidence accepted an invalid override with --module-cache"
fi
/usr/bin/grep -Fq "TESLATLAS_GO does not exist: $missing_override" \
    "$TMP/explicit-cache.out" \
    || fail "--module-cache bypassed explicit Go validation"
[ ! -e "$evidence_fallback_log" ] \
    || fail "explicit-cache invalid override silently fell back to ambient Go"

/bin/mkdir -p "$TMP/empty-bin"
if PATH="$TMP/empty-bin" /usr/bin/python3 "$FLEET_EVIDENCE" --repo "$ROOT" \
    --receiver-binary "$TMP/missing-receiver" --module-cache "$module_cache" \
    --output-dir "$TMP/no-go-explicit-cache-output" \
    >"$TMP/no-go-explicit-cache.out" 2>&1; then
    fail "Fleet no-Go fixture unexpectedly generated evidence"
fi
/usr/bin/grep -Fq 'Fleet Telemetry receiver is missing' \
    "$TMP/no-go-explicit-cache.out" \
    || fail "explicit module cache incorrectly requires Go without an override"

default_log="$TMP/default.log"
default_go="$TMP/default/bin/go"
make_fake_go "$default_go" go1.27.0 "$default_log" "$module_cache"
default_selected=$(PATH="$(/usr/bin/dirname "$default_go"):/usr/bin:/bin" \
    /usr/bin/python3 "$HELPER" --expected-version go1.27.0) \
    || fail "default PATH selection was rejected"
default_expected=$(/usr/bin/python3 -c \
    'import os, sys; print(os.path.abspath(sys.argv[1]))' "$default_go")
[ "$default_selected" = "$default_expected" ] || fail "default PATH selection changed"

wrong_log="$TMP/wrong.log"
wrong_go="$TMP/wrong/bin/go"
fallback_log="$TMP/fallback.log"
fallback_go="$TMP/fallback/bin/go"
make_fake_go "$wrong_go" go1.27.1 "$wrong_log" "$module_cache"
make_fake_go "$fallback_go" go1.27.0 "$fallback_log" "$module_cache"
if PATH="$(/usr/bin/dirname "$fallback_go"):/usr/bin:/bin" \
    TESLATLAS_GO="$wrong_go" \
    /usr/bin/python3 "$HELPER" --expected-version go1.27.0 \
    >"$TMP/wrong.out" 2>&1; then
    fail "wrong-version explicit Go was accepted"
fi
/usr/bin/grep -Fq 'go1.27.0 is required exactly: go1.27.1' "$TMP/wrong.out" \
    || fail "wrong-version failure was not exact"
[ -s "$wrong_log" ] || fail "wrong-version override was not checked"
[ ! -e "$fallback_log" ] || fail "wrong-version override silently fell back"

non_executable="$TMP/not executable/go"
/bin/mkdir -p "$(/usr/bin/dirname "$non_executable")"
/usr/bin/printf '%s\n' '#!/bin/sh' 'exit 0' >"$non_executable"
/bin/chmod 0600 "$non_executable"
malformed_go=$(/usr/bin/printf '%s\n' "$TMP/malformed" go)
for invalid_go in '' relative/go "$malformed_go" "$TMP/missing/go" "$non_executable"; do
    /bin/rm -f "$fallback_log"
    if PATH="$(/usr/bin/dirname "$fallback_go"):/usr/bin:/bin" \
        TESLATLAS_GO="$invalid_go" \
        /usr/bin/python3 "$HELPER" --expected-version go1.27.0 \
        >"$TMP/invalid.out" 2>&1; then
        fail "invalid explicit Go was accepted: $invalid_go"
    fi
    [ ! -e "$fallback_log" ] || fail "invalid override silently fell back: $invalid_go"
done

/usr/bin/printf '%s\n' 'Go toolchain selection tests passed'
