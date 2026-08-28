#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
HELPER="$ROOT/scripts/rust-source-evidence.py"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-rust-source-evidence.XXXXXX")

if [ -x /usr/bin/python3 ]; then
  PYTHONDONTWRITEBYTECODE=1 /usr/bin/python3 - "$HELPER" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("rust_source_python39", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
assert module.load_lock
PY
fi

cleanup() {
  chmod -R u+w "$TMP" 2>/dev/null || true
  find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

expect_failure() {
  if "$@" >/dev/null 2>&1; then
    printf '%s\n' "expected failure: $*" >&2
    exit 1
  fi
}

mkdir -p "$TMP/repo/src"
cat >"$TMP/repo/Cargo.toml" <<'EOF'
[package]
name = "rust-source-fixture"
version = "0.1.0"
edition = "2024"
rust-version = "1.98"

[[bin]]
name = "rust-source-fixture"
path = "src/main.rs"

[dependencies]
itoa = "=1.0.18"
EOF
cat >"$TMP/repo/src/main.rs" <<'EOF'
fn main() {
    println!("{}", itoa::Buffer::new().format(42));
}
EOF

(cd "$TMP/repo" && cargo generate-lockfile --offline)
if [ -x /usr/bin/python3 ]; then
  PYTHONDONTWRITEBYTECODE=1 /usr/bin/python3 - "$HELPER" "$TMP/repo" <<'PY'
import importlib.util
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location("rust_source_parser_python39", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
_, registry, workspace = module.load_lock(Path(sys.argv[2]))
assert [(item["name"], item["version"]) for item in registry] == [("itoa", "1.0.18")]
assert workspace == [("rust-source-fixture", "0.1.0")]
assert module.parse_root_manifest_identity(
    b'[package]\nname = "fixture"\nversion = "1.2.3-beta.4"\n'
) == ("fixture", "1.2.3-beta.4")
for invalid_manifest in (
    b'[package]\nname = "fixture"\nname = "duplicate"\nversion = "1.0.0"\n',
    b'[package]\nname = "fixture"\nversion = { workspace = true }\n',
    b'[package]\nname = "fixture"\ndescription = """bad"""\nversion = "1.0.0"\n',
    b'[package]\nname = "fixture"\nversion = "1.0.0"',
):
    try:
        module.parse_root_manifest_identity(invalid_manifest)
    except module.GateError:
        pass
    else:
        raise AssertionError(f"unsafe Cargo.toml accepted: {invalid_manifest!r}")
for invalid_lock in (
    b'version = 3\n\n[[package]]\nname = "fixture"\nversion = "1.0.0"\n',
    b'version = 4\n\n[[package]]\nname = "fixture"\nname = "duplicate"\nversion = "1.0.0"\n',
    b'version = 4\n\n[[package]]\nname = "fixture"\nversion = "1.0.0"',
    b'version = 4\n\n[[package]]\nname = "fixture"\ndescription = """bad"""\nversion = "1.0.0"\n',
):
    try:
        module.parse_lock_toml(invalid_lock)
    except module.GateError:
        pass
    else:
        raise AssertionError(f"unsafe Cargo.lock accepted: {invalid_lock!r}")
PY
fi
python3 "$HELPER" \
  --repo "$TMP/repo" \
  --cargo-home "${CARGO_HOME:-$HOME/.cargo}" \
  --bin rust-source-fixture \
  --output-dir "$TMP/evidence-a" >/dev/null
python3 "$HELPER" \
  --repo "$TMP/repo" \
  --cargo-home "${CARGO_HOME:-$HOME/.cargo}" \
  --bin rust-source-fixture \
  --output-dir "$TMP/evidence-b" >/dev/null

CARGO_HOME="$TMP/unavailable-after-generation" \
  python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/evidence-a"
if [ -x /usr/bin/python3 ]; then
  CARGO_HOME="$TMP/unavailable-after-generation" \
    PYTHONDONTWRITEBYTECODE=1 /usr/bin/python3 "$HELPER" \
      --repo "$TMP/repo" --verify-dir "$TMP/evidence-a"
fi
diff -qr "$TMP/evidence-a" "$TMP/evidence-b"

python3 - "$TMP/evidence-a" <<'PY'
import json
import pathlib
import sys
import tarfile

evidence = pathlib.Path(sys.argv[1])
assert {item.name for item in evidence.iterdir()} == {
    "rust-vendored-sources.tar.gz",
    "rust-source-inventory.json",
    "rust-source-evidence-manifest.json",
}
inventory = json.loads((evidence / "rust-source-inventory.json").read_text())
manifest = json.loads((evidence / "rust-source-evidence-manifest.json").read_text())
assert inventory["dependency_count"] == 1
assert inventory["packages"][0]["name"] == "itoa"
assert inventory["packages"][0]["version"] == "1.0.18"
assert manifest["offline_locked_build"]["passed"] is True
assert manifest["offline_locked_build"]["command"] == [
    "cargo", "build", "--locked", "--offline", "--release", "--bin", "rust-source-fixture"
]
with tarfile.open(evidence / "rust-vendored-sources.tar.gz", "r:gz") as archive:
    names = {member.name for member in archive}
assert "crate-archives/itoa-1.0.18.crate" in names
assert "vendor/itoa-1.0.18/.cargo-checksum.json" in names
PY

cp -R "$TMP/evidence-a" "$TMP/tampered"
chmod u+w "$TMP/tampered/rust-source-inventory.json"
printf ' ' >>"$TMP/tampered/rust-source-inventory.json"
expect_failure python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/tampered"

cp -R "$TMP/evidence-a" "$TMP/symlinked"
mv "$TMP/symlinked/rust-vendored-sources.tar.gz" "$TMP/saved-archive"
ln -s ../saved-archive "$TMP/symlinked/rust-vendored-sources.tar.gz"
expect_failure python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/symlinked"

cp -R "$TMP/evidence-a" "$TMP/extra"
printf '%s\n' extra >"$TMP/extra/unreviewed"
expect_failure python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/extra"

mutate_archive() {
  destination=$1
  operation=$2
  cp -R "$TMP/evidence-a" "$destination"
  python3 - "$HELPER" "$destination" "$operation" <<'PY'
import importlib.util
import json
import os
import pathlib
import shutil
import sys
import tempfile

helper, evidence, operation = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
spec = importlib.util.spec_from_file_location("rust_source_evidence", helper)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)
work = pathlib.Path(tempfile.mkdtemp(dir=evidence.parent))
try:
    archive = evidence / module.ARCHIVE_NAME
    module.extract_archive(archive.read_bytes(), work)
    crates = work / module.CRATE_ARCHIVE_DIRECTORY
    locked = crates / "itoa-1.0.18.crate"
    if operation == "omit":
        locked.unlink()
    elif operation == "extra":
        module.write_new(crates / "unlocked-9.9.9.crate", b"unlocked")
    elif operation == "wrong":
        os.chmod(locked, 0o600)
        locked.write_bytes(locked.read_bytes() + b"forged")
        os.chmod(locked, 0o444)
    elif operation == "forge":
        inventory_path = evidence / module.INVENTORY_NAME
        inventory = json.loads(inventory_path.read_text())
        package_record = inventory["packages"][0]
        package = work / "vendor" / package_record["vendor_path"]
        target = package / "Cargo.toml"
        os.chmod(target, 0o600)
        target.write_bytes(target.read_bytes() + b"\n# forged evidence tree\n")
        os.chmod(target, 0o444)
        files = {}
        expanded_size = 0
        for path in sorted(package.rglob("*")):
            if path.is_file() and path.name != ".cargo-checksum.json":
                relative = path.relative_to(package).as_posix()
                data = path.read_bytes()
                files[relative] = module.sha256_bytes(data)
                expanded_size += len(data)
        checksum_data = module.json_bytes(
            {"files": dict(sorted(files.items())), "package": package_record["checksum"]}
        )
        checksum_path = package / ".cargo-checksum.json"
        os.chmod(checksum_path, 0o600)
        checksum_path.write_bytes(checksum_data)
        os.chmod(checksum_path, 0o444)
        package_record.update(
            {
                "file_count": len(files),
                "expanded_size": expanded_size,
                "tree_sha256": module.tree_hash(files),
                "cargo_checksum_sha256": module.sha256_bytes(checksum_data),
            }
        )
        os.chmod(inventory_path, 0o600)
        inventory_path.write_bytes(module.json_bytes(inventory))
    else:
        raise AssertionError(operation)
    os.chmod(archive, 0o600)
    archive.unlink()
    module.write_archive(work, archive)
    archive_data = archive.read_bytes()
    manifest_path = evidence / module.MANIFEST_NAME
    manifest = json.loads(manifest_path.read_text())
    inventory_data = (evidence / module.INVENTORY_NAME).read_bytes()
    manifest["inventory_sha256"] = module.sha256_bytes(inventory_data)
    manifest["vendor_archive_sha256"] = module.sha256_bytes(archive_data)
    manifest["vendor_archive_size"] = len(archive_data)
    os.chmod(manifest_path, 0o600)
    manifest_path.write_bytes(module.json_bytes(manifest))
finally:
    shutil.rmtree(work, ignore_errors=True)
PY
}

mutate_archive "$TMP/omitted-crate" omit
expect_failure python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/omitted-crate"
mutate_archive "$TMP/extra-crate" extra
expect_failure python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/extra-crate"
mutate_archive "$TMP/wrong-crate" wrong
expect_failure python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/wrong-crate"
mutate_archive "$TMP/forged-tree" forge
if python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/forged-tree" \
    >"$TMP/forged-tree.out" 2>&1; then
    printf '%s\n' 'forged vendor evidence was accepted' >&2
    exit 1
fi
grep -Eq 'not derived from Cargo.lock|differs from locked crate reconstruction' \
    "$TMP/forged-tree.out"

cp -R "$TMP/evidence-a" "$TMP/hardlinked"
rm "$TMP/hardlinked/rust-source-inventory.json"
cp "$TMP/evidence-a/rust-source-inventory.json" "$TMP/hardlink-target"
ln "$TMP/hardlink-target" "$TMP/hardlinked/rust-source-inventory.json"
expect_failure python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/hardlinked"

mkdir "$TMP/empty-cargo-home"
expect_failure python3 "$HELPER" \
  --repo "$TMP/repo" \
  --cargo-home "$TMP/empty-cargo-home" \
  --bin rust-source-fixture \
  --output-dir "$TMP/missing-source"

mkdir "$TMP/symlink-cargo-home"
ln -s "${CARGO_HOME:-$HOME/.cargo}/registry" "$TMP/symlink-cargo-home/registry"
expect_failure python3 "$HELPER" \
  --repo "$TMP/repo" \
  --cargo-home "$TMP/symlink-cargo-home" \
  --bin rust-source-fixture \
  --output-dir "$TMP/symlink-source"

cp "$TMP/repo/Cargo.lock" "$TMP/original-lock"
chmod u+w "$TMP/repo/Cargo.lock"
printf '%s\n' '# tamper' >>"$TMP/repo/Cargo.lock"
expect_failure python3 "$HELPER" --repo "$TMP/repo" --verify-dir "$TMP/evidence-a"
cp "$TMP/original-lock" "$TMP/repo/Cargo.lock"

mkdir -p "$TMP/path-dependency/src"
cat >"$TMP/path-dependency/Cargo.toml" <<'EOF'
[package]
name = "outside-repository"
version = "0.1.0"
edition = "2024"
EOF
printf '%s\n' 'pub fn value() -> u8 { 1 }' >"$TMP/path-dependency/src/lib.rs"
cat >>"$TMP/repo/Cargo.toml" <<EOF
outside-repository = { path = "$TMP/path-dependency" }
EOF
(cd "$TMP/repo" && cargo generate-lockfile --offline)
expect_failure python3 "$HELPER" \
  --repo "$TMP/repo" \
  --cargo-home "${CARGO_HOME:-$HOME/.cargo}" \
  --bin rust-source-fixture \
  --output-dir "$TMP/path-source"

printf '%s\n' 'rust source evidence tests passed'
