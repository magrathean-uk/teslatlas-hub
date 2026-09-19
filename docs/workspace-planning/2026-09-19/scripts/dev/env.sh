#!/bin/bash
# Source this file through run.sh for Teslatlas development commands.
export TESLATLAS_LAB="${TESLATLAS_LAB:-$HOME/dev/teslatlas-lab}"
export CARGO_HOME="$HOME/dev/toolchains/rust/cargo"
export RUSTUP_HOME="$HOME/dev/toolchains/rust/rustup"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
export npm_config_cache="$TESLATLAS_LAB/cache/npm"
export PIP_CACHE_DIR="$TESLATLAS_LAB/cache/pip"
export UV_CACHE_DIR="$TESLATLAS_LAB/cache/uv"
export GOCACHE="$TESLATLAS_LAB/cache/go-build"
export GOMODCACHE="$TESLATLAS_LAB/cache/go-mod"
export TMPDIR="$TESLATLAS_LAB/tmp/"
export TART_NO_AUTO_PRUNE=1
export PATH="$CARGO_HOME/bin:$PATH"

