#!/bin/sh
# Refresh the Hub's pinned SQLite amalgamation from the official SQLite site.
# This is release-engineering only; ordinary Hub builds compile the committed
# vendored source and never download SQLite at build or install time.
set -eu

sqlite_version=3530400
archive_sha3=628a44cfe82c66aed1ccbbe85a562d2e33ebe64b3288981ed76285612227934e
sqlite_c_sha3=67f423e9ebbbdc473cbc4772c872ee6b89f31fde4ed0279a5c25d5f65c043a16
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)
vendor_dir=$repo_dir/vendor/libsqlite3-sys
cache_dir=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-sqlite.XXXXXX")
archive=$cache_dir/sqlite-amalgamation-$sqlite_version.zip
source_dir=$cache_dir/sqlite-amalgamation-$sqlite_version
bindgen_target=$cache_dir/cargo-target
registry_root=${CARGO_HOME:-$HOME/.cargo}/registry/src

cleanup() {
    rm -rf "$cache_dir"
}
trap cleanup EXIT HUP INT TERM

sha3_256_file() {
    if openssl list -digest-algorithms 2>/dev/null | grep -qi 'sha3-256'; then
        openssl dgst -sha3-256 -r "$1" | awk '{print $1}'
        return
    fi
    # Debian's Python has SHA-3 in hashlib. This path is only for refreshing
    # the vendor tree on hosts with LibreSSL, not for normal Hub operation.
    python3 - "$1" <<'PY'
import hashlib
import pathlib
import sys

print(hashlib.sha3_256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
}

if [ -e "$vendor_dir" ]; then
    echo "refusing to overwrite existing $vendor_dir" >&2
    exit 1
fi

crate_source=$(find "$registry_root" -type d -path '*/libsqlite3-sys-0.38.1' -print -quit)
if [ -z "$crate_source" ]; then
    echo "libsqlite3-sys 0.38.1 is not available in the local Cargo registry" >&2
    exit 1
fi

mkdir -p "$cache_dir" "$repo_dir/vendor"
curl --fail --location --proto '=https' --tlsv1.2 \
    "https://www.sqlite.org/2026/sqlite-amalgamation-$sqlite_version.zip" \
    --output "$archive"
actual_archive_sha3=$(sha3_256_file "$archive")
if [ "$actual_archive_sha3" != "$archive_sha3" ]; then
    echo "SQLite archive SHA3-256 mismatch" >&2
    exit 1
fi
unzip -q "$archive" -d "$cache_dir"
actual_c_sha3=$(sha3_256_file "$source_dir/sqlite3.c")
if [ "$actual_c_sha3" != "$sqlite_c_sha3" ]; then
    echo "SQLite sqlite3.c SHA3-256 mismatch" >&2
    exit 1
fi

cp -R "$crate_source" "$vendor_dir"
cp "$source_dir/sqlite3.c" "$source_dir/sqlite3.h" "$source_dir/sqlite3ext.h" "$vendor_dir/sqlite3/"

# Regenerate the committed, non-extension bindings from those exact headers.
# LIBCLANG_PATH may be supplied for Xcode or an alternate LLVM install.
(
    cd "$vendor_dir"
    CARGO_TARGET_DIR="$bindgen_target" LIBSQLITE3_SYS_BUNDLING=1 \
        cargo build --locked --features 'bundled,buildtime_bindgen'
)
binding=$(find "$bindgen_target" -path '*/out/bindgen.rs' -type f -print -quit)
if [ -z "$binding" ]; then
    echo "bindgen output was not produced" >&2
    exit 1
fi
cp "$binding" "$vendor_dir/sqlite3/bindgen_bundled_version.rs"

echo "vendored SQLite 3.53.4 into $vendor_dir"
