#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-legal-bundle-test.XXXXXX")
cleanup() {
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

REPO="$TMP/repo"
mkdir -p "$REPO/scripts" "$REPO/src"
cp "$ROOT/scripts/legal-bundle.py" "$REPO/scripts/"
cp "$ROOT/scripts/release-evidence.py" "$REPO/scripts/"
cat >"$REPO/Cargo.toml" <<'EOF'
[package]
name = "legal-fixture"
version = "1.0.0"
edition = "2024"
license = "MIT"
EOF
printf '%s\n' 'fn main() {}' >"$REPO/src/main.rs"
printf '%s\n' 'exact fixture license' >"$REPO/LICENSE"
mkdir -p "$REPO/docs/legal"
printf '%s\n' '# exact fixture notices' >"$REPO/docs/legal/third-party-notices.md"
(cd "$REPO" && CARGO_NET_OFFLINE=true cargo generate-lockfile >/dev/null)

python3 "$REPO/scripts/legal-bundle.py" --repo "$REPO" \
    --output-dir "$TMP/bundle" >/dev/null
python3 "$REPO/scripts/legal-bundle.py" --repo "$REPO" \
    --verify-dir "$TMP/bundle"

cp -R "$TMP/bundle" "$TMP/omitted"
rm "$TMP/omitted/rust-sbom.spdx.json"
if python3 "$REPO/scripts/legal-bundle.py" --repo "$REPO" \
    --verify-dir "$TMP/omitted" >"$TMP/omitted.out" 2>&1; then
    echo 'legal-bundle test: omitted component was accepted' >&2
    exit 1
fi
grep -Fq 'file set is incomplete or unexpected' "$TMP/omitted.out"

cp -R "$TMP/bundle" "$TMP/tampered"
printf '%s\n' changed >>"$TMP/tampered/RUST_THIRD_PARTY_NOTICES.generated.md"
if python3 "$REPO/scripts/legal-bundle.py" --repo "$REPO" \
    --verify-dir "$TMP/tampered" >"$TMP/tampered.out" 2>&1; then
    echo 'legal-bundle test: tampered component was accepted' >&2
    exit 1
fi
grep -Fq 'component mismatch' "$TMP/tampered.out"

cp -R "$TMP/bundle" "$TMP/symlinked"
rm "$TMP/symlinked/rust-dependency-inventory.json"
ln -s "$TMP/bundle/rust-dependency-inventory.json" \
    "$TMP/symlinked/rust-dependency-inventory.json"
if python3 "$REPO/scripts/legal-bundle.py" --repo "$REPO" \
    --verify-dir "$TMP/symlinked" >"$TMP/symlinked.out" 2>&1; then
    echo 'legal-bundle test: symlinked component was accepted' >&2
    exit 1
fi
grep -Fq 'regular non-symlink file' "$TMP/symlinked.out"

cp -R "$TMP/bundle" "$TMP/hardlinked"
rm "$TMP/hardlinked/rust-dependency-inventory.json"
cp "$TMP/bundle/rust-dependency-inventory.json" "$TMP/hardlink-target"
ln "$TMP/hardlink-target" "$TMP/hardlinked/rust-dependency-inventory.json"
if python3 "$REPO/scripts/legal-bundle.py" --repo "$REPO" \
    --verify-dir "$TMP/hardlinked" >"$TMP/hardlinked.out" 2>&1; then
    echo 'legal-bundle test: hardlinked component was accepted' >&2
    exit 1
fi
grep -Fq 'regular non-symlink file' "$TMP/hardlinked.out"

printf '%s\n' 'changed fixture license' >"$REPO/LICENSE"
if python3 "$REPO/scripts/legal-bundle.py" --repo "$REPO" \
    --verify-dir "$TMP/bundle" >"$TMP/mismatch.out" 2>&1; then
    echo 'legal-bundle test: source mismatch was accepted' >&2
    exit 1
fi
grep -Fq 'component mismatch' "$TMP/mismatch.out"

python3 - "$ROOT/scripts/release-evidence.py" "$TMP" <<'PY'
import importlib.util
import os
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location("release_evidence", sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
root = Path(sys.argv[2]) / "unsafe-licenses"
package = root / "package"
package.mkdir(parents=True)
(package / "Cargo.toml").write_text("[package]\nname='unsafe'\nversion='1.0.0'\n")
outside = root / "secret"
outside.write_text("private host file\n")

def rejected(license_file):
    value = {
        "name": "unsafe",
        "manifest_path": str(package / "Cargo.toml"),
        "license_file": license_file,
    }
    try:
        module.package_local_legal_paths(value, root)
    except module.GateError:
        return
    raise AssertionError(f"unsafe license path accepted: {license_file}")

rejected("../secret")
rejected(str(outside))
(package / "LICENSE").symlink_to(outside)
rejected(None)
(package / "LICENSE").unlink()
os.link(outside, package / "LICENSE")
rejected(None)
PY

printf '%s\n' 'legal-bundle fail-closed test passed'
