#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

usage() {
    cat >&2 <<'EOF'
usage: scripts/bootstrap-dev.sh --prefix ABSOLUTE_PATH --components none [--dry-run]

Hub-only development setup is available here. Use bootstrap-companions.py and
the reviewed catalog flow for companion components.
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

[ -n "$prefix" ] && [ "$components" = none ] || usage
case "$prefix" in /*) ;; *) usage ;; esac

if [ "$dry_run" = true ]; then
    python3 - "$prefix" <<'PY'
import json
import sys

print(json.dumps({
    "schema": "teslatlas.bootstrap-dev-plan/v1",
    "prefix": sys.argv[1],
    "components": ["hub-core"],
    "companion_components": [],
}, sort_keys=True))
PY
    exit 0
fi

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

python3 - "$hub_version" "$hub_url" <<'PY'
import json
import sys

print(json.dumps({
    "schema": "teslatlas.bootstrap-dev-result/v1",
    "status": "hub-core-ready",
    "hub_version": sys.argv[1],
    "hub_url": sys.argv[2],
}, sort_keys=True))
PY
