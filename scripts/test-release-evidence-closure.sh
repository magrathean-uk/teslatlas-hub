#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Tests for the helpers scripts/release-evidence.py keeps for scripts/legal-bundle.py
# (cargo_lock_checksums, normalize_spdx_expression, sbom_and_notices,
# package_license_material, spdx_identifiers, regular_file and the witness helpers
# capture_artifact and witness_bytes, including their symlink and hard-link
# refusals), and the substituted-bridge-lock rejection of
# scripts/fleet-telemetry-evidence.py. These were the unit and negative tests of the
# retired release-evidence generator.
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
HELPER="$ROOT/scripts/release-evidence.py"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-release-evidence-closure.XXXXXX")
PYTHONDONTWRITEBYTECODE=1
export PYTHONDONTWRITEBYTECODE

cleanup() {
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

# macOS ships Python 3.9: the helper must keep loading there, exactly as
# scripts/legal-bundle.py loads it (exec_module, so it must have no side effects).
if [ -x /usr/bin/python3 ]; then
    /usr/bin/python3 - "$HELPER" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("release_evidence_python39", sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
assert module.cargo_metadata
assert module.cargo_lock_checksums
assert module.sbom_and_notices
assert module.package_local_legal_paths
PY
fi

python3 - "$HELPER" "$TMP" <<'PY'
import hashlib
import importlib.util
import os
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location("release_evidence_test", sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
root = Path(sys.argv[2])

# The module is loaded by scripts/legal-bundle.py and calls only these two entry points.
assert callable(module.cargo_metadata)
assert callable(module.sbom_and_notices)


REFUSED = "regular, non-symlink file"


def rejected_with(message, function, *args):
    """The call must raise GateError whose text contains the exact message."""
    try:
        function(*args)
    except module.GateError as error:
        assert message in str(error), f"wrong rejection ({error}), wanted: {message}"
    else:
        raise AssertionError(f"unsafe input accepted: {function.__name__}{args!r}")

toml_repo = root / "strict-toml-repo"
toml_repo.mkdir()
lock = toml_repo / "Cargo.lock"
lock.write_text(
    "# generated\nversion = 4\n\n"
    "[[package]]\nname = \"fixture\"\nversion = \"1.0.0\"\n\n"
    "[[package]]\nname = \"registry-dependency\"\nversion = \"2.0.0\"\n"
    "source = \"registry+https://example.invalid/index\"\n"
    f"checksum = \"{'a' * 64}\"\n"
    "dependencies = [\n \"fixture\",\n]\n\n"
    "[[package]]\nname = \"git-dependency\"\nversion = \"3.0.0\"\n"
    "source = \"git+https://example.invalid/repo#0123456789abcdef\"\n"
)
assert module.cargo_lock_checksums(toml_repo) == {
    (
        "registry-dependency",
        "2.0.0",
        "registry+https://example.invalid/index",
    ): "a" * 64
}
for invalid_lock in (
    "version = 2\n\n[[package]]\nname = \"fixture\"\nversion = \"1.0.0\"\n",
    "version = 4\n\n[[package]]\nname = \"fixture\"\nname = \"other\"\nversion = \"1.0.0\"\n",
    "version = 4\n\n[[package]]\nname = \"registry\"\nversion = \"1.0.0\"\nsource = \"registry+https://example.invalid/index\"\n",
    "version = 4\n\n[[package]]\nname = \"registry\"\nversion = \"1.0.0\"\nsource = \"registry+https://example.invalid/index\"\nchecksum = \"invalid\"\n",
    "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"1.0.0\"",
):
    lock.write_text(invalid_lock)
    try:
        module.cargo_lock_checksums(toml_repo)
    except module.GateError:
        pass
    else:
        raise AssertionError(f"unsafe Cargo lock accepted: {invalid_lock!r}")

source = root / "witness-source"
source.write_bytes(b"descriptor-pinned evidence\n")
witness = module.capture_artifact(root, source)
assert witness.relative_path == "witness-source"
assert witness.size == len(b"descriptor-pinned evidence\n")
assert witness.digest == hashlib.sha256(b"descriptor-pinned evidence\n").hexdigest()
assert module.witness_bytes(witness, "witness", 1024) == source.read_bytes()
replacement = root / "witness-replacement"
replacement.write_bytes(b"replacement evidence\n")
source.rename(root / "witness-source-original")
replacement.rename(source)
try:
    module.witness_bytes(witness, "witness", 1024)
except module.GateError:
    pass
else:
    raise AssertionError("replaced evidence path was read after capture")
link = root / "witness-link"
link.symlink_to(source)
try:
    module.capture_artifact(root, link)
except module.GateError:
    pass
else:
    raise AssertionError("symlinked evidence path was captured")
# capture_artifact accepts only a regular file with exactly one link: a hard link, a
# symlink, a directory and a missing path are each refused, with the exact reason.
hard_source = root / "witness-hardlink-source"
hard_source.write_bytes(b"hardlinked evidence\n")
assert module.capture_artifact(root, hard_source).size == len(b"hardlinked evidence\n")
os.link(hard_source, root / "witness-hardlink")
rejected_with(REFUSED, module.capture_artifact, root, hard_source)
rejected_with(REFUSED, module.capture_artifact, root, root / "witness-hardlink")
rejected_with(REFUSED, module.capture_artifact, root, link)
rejected_with(REFUSED, module.capture_artifact, root, root)
rejected_with("artifact is unavailable", module.capture_artifact, root,
              root / "witness-missing")

# regular_file guards the SPDX corpus texts, the project LICENSE and the notices file.
plain = root / "regular-file-plain"
plain.write_text("plain\n")
assert module.regular_file(plain, "plain evidence") is None
plain_link = root / "regular-file-link"
plain_link.symlink_to(plain)
for refused_path in (root / "regular-file-missing", plain_link, root):
    rejected_with(
        f"plain evidence must be a {REFUSED}: {refused_path}",
        module.regular_file, refused_path, "plain evidence",
    )

# cargo_lock_checksums reads Cargo.lock through capture_artifact: a hard-linked or
# symlinked lock is refused, the same lock as a plain file is accepted.
hard_lock_repo = root / "hardlinked-lock-repo"
hard_lock_repo.mkdir()
hard_lock = hard_lock_repo / "Cargo.lock"
hard_lock.write_text("version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"1.0.0\"\n")
assert module.cargo_lock_checksums(hard_lock_repo) == {}
os.link(hard_lock, root / "hardlinked-lock-peer")
rejected_with(REFUSED, module.cargo_lock_checksums, hard_lock_repo)
symlink_lock_repo = root / "symlinked-lock-repo"
symlink_lock_repo.mkdir()
(symlink_lock_repo / "Cargo.lock").symlink_to(root / "hardlinked-lock-peer")
rejected_with(REFUSED, module.cargo_lock_checksums, symlink_lock_repo)

assert module.normalize_spdx_expression("MIT/Apache-2.0", "legacy") == \
    "MIT OR Apache-2.0"
assert module.normalize_spdx_expression("Apache-2.0 / MIT", "fnv") == \
    "Apache-2.0 OR MIT"
assert module.normalize_spdx_expression("Unlicense/MIT", "walkdir") == \
    "Unlicense OR MIT"
for invalid_expression in (
    "MIT, Apache-2.0", "GPL-2.0+", "MIT OR", "BSD-3-Clause/MIT",
    "MIT / Apache-2.0",
):
    try:
        module.normalize_spdx_expression(invalid_expression, "invalid")
    except module.GateError:
        pass
    else:
        raise AssertionError(f"ambiguous SPDX expression accepted: {invalid_expression}")

sbom_repo = root / "sbom-checksum-repo"
root_package = sbom_repo / "root-package"
dependency = sbom_repo / "registry-dependency"
root_package.mkdir(parents=True)
dependency.mkdir()
(sbom_repo / "docs/legal").mkdir(parents=True)
(sbom_repo / "docs/legal/third-party-notices.md").write_text("# notices\n")
(root_package / "Cargo.toml").write_text(
    '[package]\nname = "fixture"\nversion = "1.0.0"\nlicense = "MIT"\n'
)
(root_package / "LICENSE").write_text("MIT root\n")
(dependency / "Cargo.toml").write_text(
    '[package]\nname = "registry-dependency"\nversion = "2.3.4"\n'
    'license = "MIT/Apache-2.0"\n'
)
registry_source = "registry+https://github.com/rust-lang/crates.io-index"
registry_checksum = "a" * 64
(sbom_repo / "Cargo.lock").write_text(
    "# generated fixture\nversion = 4\n\n"
    "[[package]]\nname = \"fixture\"\nversion = \"1.0.0\"\n\n"
    "[[package]]\nname = \"registry-dependency\"\nversion = \"2.3.4\"\n"
    f"source = \"{registry_source}\"\nchecksum = \"{registry_checksum}\"\n"
)
root_id = "path+file:///fixture#fixture@1.0.0"
dependency_id = f"{registry_source}#registry-dependency@2.3.4"
metadata = {
    "packages": [
        {
            "id": root_id,
            "name": "fixture",
            "version": "1.0.0",
            "source": None,
            "checksum": None,
            "manifest_path": str(root_package / "Cargo.toml"),
            "license": "MIT",
            "license_file": None,
            "repository": None,
        },
        {
            "id": dependency_id,
            "name": "registry-dependency",
            "version": "2.3.4",
            "source": registry_source,
            "checksum": None,
            "manifest_path": str(dependency / "Cargo.toml"),
            "license": "MIT/Apache-2.0",
            "license_file": None,
            "repository": "https://example.invalid/dependency",
        },
    ],
    "workspace_members": [root_id],
    "resolve": {
        "root": root_id,
        "nodes": [
            {"id": root_id, "deps": [{"pkg": dependency_id}]},
            {"id": dependency_id, "deps": []},
        ],
    },
}
try:
    module.sbom_and_notices(metadata, sbom_repo)
except module.GateError as error:
    assert "SPDX corpus is missing" in str(error)
else:
    raise AssertionError("missing SPDX license corpus was accepted")
(sbom_repo / "LICENSES").mkdir()
(sbom_repo / "LICENSES/MIT.txt").write_text("MIT corpus fixture\n")
(sbom_repo / "LICENSES/Apache-2.0.txt").write_text("Apache corpus fixture\n")
spdx, inventory, notices = module.sbom_and_notices(metadata, sbom_repo)
dependency_spdx = next(
    package for package in spdx["packages"]
    if package["name"] == "registry-dependency"
)
assert dependency_spdx["licenseDeclared"] == "MIT OR Apache-2.0"
assert dependency_spdx["checksums"] == [
    {"algorithm": "SHA256", "checksumValue": registry_checksum}
]
dependency_inventory = next(
    package for package in inventory["packages"]
    if package["name"] == "registry-dependency"
)
assert dependency_inventory["checksum"] == registry_checksum
assert dependency_inventory["license_original"] == "MIT/Apache-2.0"
assert dependency_inventory["license_text_sources"] == [
    "LICENSES/MIT.txt", "LICENSES/Apache-2.0.txt"
]
assert dependency_inventory["package_notice"] == "absent-in-crate-archive"
assert "MIT corpus fixture\nApache corpus fixture\n" in notices

# A symlinked or missing SPDX corpus text and a symlinked or missing project notices
# file are refused (read_text would follow the link, so only regular_file stops them).
corpus_mit = sbom_repo / "LICENSES/MIT.txt"
outside_mit = root / "sbom-outside-MIT.txt"
outside_mit.write_text("MIT corpus fixture\n")
corpus_mit.unlink()
corpus_mit.symlink_to(outside_mit)
rejected_with(f"SPDX corpus license MIT must be a {REFUSED}",
              module.sbom_and_notices, metadata, sbom_repo)
corpus_mit.unlink()
rejected_with(f"SPDX corpus license MIT must be a {REFUSED}",
              module.sbom_and_notices, metadata, sbom_repo)
corpus_mit.write_text("MIT corpus fixture\n")
module.sbom_and_notices(metadata, sbom_repo)
project_notices = sbom_repo / "docs/legal/third-party-notices.md"
outside_notices = root / "sbom-outside-notices.md"
outside_notices.write_text("# notices\n")
project_notices.unlink()
project_notices.symlink_to(outside_notices)
rejected_with(f"project notices must be a {REFUSED}",
              module.sbom_and_notices, metadata, sbom_repo)
project_notices.unlink()
rejected_with(f"project notices must be a {REFUSED}",
              module.sbom_and_notices, metadata, sbom_repo)
project_notices.write_text("# notices\n")
module.sbom_and_notices(metadata, sbom_repo)

project_legal_repo = root / "project-legal-selection"
project_legal_repo.mkdir()
(project_legal_repo / "Cargo.toml").write_text("[package]\nname='project'\nversion='1.0.0'\n")
(project_legal_repo / "LICENSE").write_text("exact project license\n")
(project_legal_repo / "LICENCE_VERSION_DECISION.md").write_text("not a license\n")
project_package = {"name": "project", "manifest_path": str(project_legal_repo / "Cargo.toml")}
_, project_text, project_sources, _, _ = module.package_license_material(
    project_package, project_legal_repo, "AGPL-3.0-only"
)
assert project_sources == ["LICENSE"]
assert "exact project license" in project_text
assert "not a license" not in project_text

# The project LICENSE must be a plain single-link file: a symlink or a hard link is refused.
for kind in ("symlink", "hardlink"):
    unsafe_repo = root / f"project-license-{kind}"
    unsafe_repo.mkdir()
    (unsafe_repo / "Cargo.toml").write_text("[package]\nname='project'\nversion='1.0.0'\n")
    outside_license = root / f"project-license-outside-{kind}"
    outside_license.write_text("outside license\n")
    if kind == "symlink":
        (unsafe_repo / "LICENSE").symlink_to(outside_license)
        expected = f"project license must be a {REFUSED}"
    else:
        os.link(outside_license, unsafe_repo / "LICENSE")
        expected = "project license must not be hard-linked"
    rejected_with(
        expected, module.package_license_material,
        {"name": "project", "manifest_path": str(unsafe_repo / "Cargo.toml")},
        unsafe_repo, "AGPL-3.0-only",
    )

multi_legal = root / "multi-legal-package"
multi_legal.mkdir()
(multi_legal / "Cargo.toml").write_text("[package]\nname='multi'\nversion='1.0.0'\n")
for name, content in (
    ("LICENSE", "MIT text\n"),
    ("LICENSE.httprouter", "BSD text\n"),
    ("NOTICE", "upstream notice\n"),
    ("COPYRIGHT", "Copyright Example\n"),
):
    (multi_legal / name).write_text(content)
multi_package = {"name": "multi", "manifest_path": str(multi_legal / "Cargo.toml")}
_, multi_text, multi_sources, multi_notices, multi_copyright = \
    module.package_license_material(multi_package, root, "MIT AND BSD-3-Clause")
assert multi_sources == ["COPYRIGHT", "LICENSE", "LICENSE.httprouter", "NOTICE"]
assert multi_notices == ["COPYRIGHT", "NOTICE"]
assert all(value in multi_text for value in ("MIT text", "BSD text", "upstream notice"))
assert multi_copyright == "Copyright Example"
assert module.spdx_identifiers("(MIT OR Apache-2.0) AND Unicode-3.0") == [
    "MIT", "Apache-2.0", "Unicode-3.0"
]
PY

# A bridge lock that no longer matches the reviewed legal lock must be rejected
# before any receiver binary, source archive or Go module cache is read.
mkdir -p "$TMP/substituted-repo/packaging/fleet-telemetry-bridge" "$TMP/module-cache"
cp "$ROOT/packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json" \
    "$TMP/substituted-repo/packaging/fleet-telemetry-bridge/"
cp "$ROOT/packaging/fleet-telemetry-bridge/fleet-telemetry-legal-lock.json" \
    "$TMP/substituted-repo/packaging/fleet-telemetry-bridge/"
printf '%s\n' 'placeholder receiver' >"$TMP/fleet-telemetry"
python3 - "$TMP/substituted-repo/packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
lock = json.loads(path.read_text())
lock["upstream"]["commit"] = "0" * 40
path.write_text(json.dumps(lock, indent=2) + "\n")
PY
if python3 "$ROOT/scripts/fleet-telemetry-evidence.py" --repo "$TMP/substituted-repo" \
    --receiver-binary "$TMP/fleet-telemetry" \
    --source-archive "$TMP/fleet-telemetry-source.tar.gz" \
    --module-cache "$TMP/module-cache" \
    --output-dir "$TMP/substituted-fleet-telemetry-evidence" \
    >"$TMP/substituted-fleet-telemetry.out" 2>&1; then
    echo 'release-evidence closure test: substituted Fleet Telemetry lock was accepted' >&2
    exit 1
fi
grep -Fq 'legal lock does not bind the bridge lock' \
    "$TMP/substituted-fleet-telemetry.out" \
    || { cat "$TMP/substituted-fleet-telemetry.out" >&2; exit 1; }
[ ! -e "$TMP/substituted-fleet-telemetry-evidence" ]

printf '%s\n' 'release-evidence closure test passed'
