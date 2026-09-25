#!/bin/bash
set -euo pipefail
dev_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dev_workspace="$(cd "$dev_script_dir/../.." && pwd)"
source "$dev_script_dir/env.sh"
if [[ $# -lt 2 ]]; then
  echo "Usage: scripts/dev/run.sh PRODUCT COMMAND [ARG ...]" >&2
  exit 2
fi
dev_product="$1"
shift
case "$dev_product" in
  hub|teslatlas-edge|teslatlas-protocol|teslatlas-sdk-typescript|teslatlas-sdk-swift|teslatlas-viewer|teslatlas-home-assistant) ;;
  *) echo "Unknown product or excluded App: $dev_product" >&2; exit 2 ;;
esac
mkdir -p "$TESLATLAS_LAB/tmp" "$TESLATLAS_LAB/cache" "$TESLATLAS_LAB/build/$dev_product/target"
export CARGO_TARGET_DIR="$TESLATLAS_LAB/build/$dev_product/target"
export CARGO_BUILD_BUILD_DIR="$CARGO_TARGET_DIR"
export TESLATLAS_DERIVED_DATA="$TESLATLAS_LAB/build/$dev_product/DerivedData"
cd "$dev_workspace/$dev_product"
if [[ "$1" == cargo ]]; then
  shift
  exec rustup run 1.98.0 cargo "$@"
fi
exec "$@"

