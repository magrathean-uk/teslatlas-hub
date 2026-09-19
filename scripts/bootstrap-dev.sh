#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

usage() {
    cat >&2 <<'EOF'
usage: scripts/bootstrap-dev.sh --prefix ABSOLUTE_PATH --components none|viewer [--dry-run]
EOF
    exit 64
}

prefix=
components=
dry_run=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix) [ "$#" -ge 2 ] || usage; prefix=$2; shift 2 ;;
        --components) [ "$#" -ge 2 ] || usage; components=$2; shift 2 ;;
        --dry-run) dry_run=true; shift ;;
        --help|-h) usage ;;
        *) usage ;;
    esac
done

[ -n "$prefix" ] && [ -n "$components" ] || usage
case "$prefix" in /*) ;; *) usage ;; esac
case "$components" in none|viewer) ;; *) usage ;; esac

if [ "$components" = viewer ]; then
    selected='hub-core,sdk-typescript,viewer'
    companions='sdk-typescript,viewer'
else
    selected='hub-core'
    companions=
fi

if [ "$dry_run" = true ]; then
    python3 - "$prefix" "$selected" "$companions" <<'PY'
import json
import sys

print(json.dumps({
    "schema": "teslatlas.bootstrap-dev-plan/v1",
    "prefix": sys.argv[1],
    "components": sys.argv[2].split(","),
    "companion_components": [] if not sys.argv[3] else sys.argv[3].split(","),
}, sort_keys=True))
PY
    exit 0
fi

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
workspace=$(CDPATH='' cd -- "$root/.." && pwd)
hub_binary=${TESLATLAS_HUB_BINARY:-teslatlas-hub}
hub_url=${TESLATLAS_HUB_URL:-https://127.0.0.1:8443}
hub_version=$($hub_binary --version) || {
    echo "installed teslatlas-hub is required" >&2
    exit 69
}
case "$hub_version" in
    'teslatlas-hub '[0-9]*.[0-9]*.[0-9]*) hub_version=${hub_version#teslatlas-hub } ;;
    *) echo "installed teslatlas-hub reported an invalid version" >&2; exit 69 ;;
esac

if [ "$components" = none ]; then
    python3 - "$hub_version" "$hub_url" <<'PY'
import json
import sys

print(json.dumps({
    "schema": "teslatlas.bootstrap-dev-result/v1",
    "status": "hub-core-ready",
    "hub_version": sys.argv[1],
    "hub_url": sys.argv[2],
    "viewer_url": None,
}, sort_keys=True))
PY
    exit 0
fi

python=${TESLATLAS_BOOTSTRAP_PYTHON:-python3}
node_bin=${TESLATLAS_NODE_BIN:-$workspace/toolchains/node-v26.7.0-linux-arm64/bin}
sdk_source=$workspace/teslatlas-sdk-typescript
viewer_source=$workspace/teslatlas-viewer
[ -d "$sdk_source" ] && [ -d "$viewer_source" ] || {
    echo "expected SDK and Viewer sibling sources are missing" >&2
    exit 69
}
[ -x "$node_bin/node" ] && [ -x "$node_bin/npm" ] || {
    echo "pinned Node and npm are required; set TESLATLAS_NODE_BIN" >&2
    exit 69
}

temporary=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-bootstrap-dev.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
manifest=$temporary/local-sources.json
catalog=$temporary/catalog.json

"$python" "$root/scripts/bootstrap-companions.py" manifest \
    --components sdk-typescript,viewer \
    --source "sdk-typescript=$sdk_source" \
    --source "viewer=$viewer_source" \
    --output "$manifest" >/dev/null
"$python" "$root/scripts/bootstrap-dev-catalog.py" \
    --local-sources "$manifest" \
    --source "sdk-typescript=$sdk_source" \
    --source "viewer=$viewer_source" \
    --hub-version "$hub_version" \
    --output "$catalog"
installation=$($hub_binary companions install \
    --mode local-candidate \
    --components sdk-typescript,viewer \
    --prefix "$prefix" \
    --catalog "$catalog" \
    --local-sources "$manifest" \
    --node-bin "$node_bin")

python3 - "$hub_version" "$hub_url" "$installation" "$prefix" <<'PY'
import json
import sys

install = json.loads(sys.argv[3])
prefix = sys.argv[4]
viewer_bin = (
    f"{prefix}/active/components/viewer/output/runtime/node_modules/"
    "teslatlas-viewer/bin/teslatlas-viewer.mjs"
)
print(json.dumps({
    "schema": "teslatlas.bootstrap-dev-result/v1",
    "status": "viewer-prepared",
    "hub_version": sys.argv[1],
    "hub_url": sys.argv[2],
    "viewer_url": "http://127.0.0.1:4173",
    "viewer_start": f"{viewer_bin} --host 127.0.0.1 --port 4173",
    "viewer_stop": "stop the Viewer process started by viewer_start",
    "installation": install,
}, sort_keys=True))
PY
