#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only

set -eu

script_directory=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
repository_root=$(CDPATH='' cd "$script_directory/.." && pwd -P)
work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/teslatlas-edge-bridge-test.XXXXXX")
cleanup() {
    /usr/bin/find "$work" -depth -delete >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=darwin-arm64 ;;
    Darwin-x86_64) target=darwin-amd64 ;;
    Linux-aarch64|Linux-arm64) target=linux-arm64 ;;
    Linux-x86_64|Linux-amd64) target=linux-amd64 ;;
    *)
        printf '%s\n' 'test-fleet-telemetry-bridge: unsupported host' >&2
        exit 1
        ;;
esac

"$repository_root/scripts/build-fleet-telemetry-bridge.sh" \
    --target "$target" \
    --output "$work/teslatlas-fleet-telemetry" >/dev/null
test -x "$work/teslatlas-fleet-telemetry"
printf '%s\n' 'fleet telemetry bridge contract: ok'
