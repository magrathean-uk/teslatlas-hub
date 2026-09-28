#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

REPO=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
SCRIPT="$REPO/scripts/fleet-telemetry-evidence.py"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-fleet-evidence.XXXXXX")

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

SOURCE=$(python3 - "$REPO" <<'PY'
import json
import os
import pathlib
import sys

repo = pathlib.Path(sys.argv[1])
lab = os.environ.get("TESLATLAS_LAB")
if not lab:
    raise SystemExit("TESLATLAS_LAB is required")
lock = json.loads(
    (repo / "packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json").read_text()
)
upstream = lock["upstream"]
print(
    pathlib.Path(lab)
    / "build"
    / "hub"
    / "upstream-cache"
    / (
        "fleet-telemetry-"
        + upstream["commit"]
        + "-"
        + upstream["archive_sha256"]
        + ".tar.gz"
    )
)
PY
)
MODULE_CACHE=$(go env GOMODCACHE)
RECEIVER=$(python3 - "$REPO" "${TESLATLAS_LAB:?TESLATLAS_LAB is required}" <<'PY'
import hashlib
import json
import pathlib
import sys

lab = pathlib.Path(sys.argv[2])
receipt = lab / "verification/workspace-dependency-maintenance-20260928T063059Z/fleet-bridge-build-receipt.json"
if not receipt.is_file():
    raise SystemExit("the current Fleet Telemetry build receipt is required")
value = json.loads(receipt.read_text())
candidate = pathlib.Path(value.get("artifact", {}).get("path", ""))
if not candidate.is_file() or candidate.is_symlink():
    raise SystemExit("the receipt-bound Fleet Telemetry receiver binary is missing or unsafe")
actual = hashlib.sha256(candidate.read_bytes()).hexdigest()
if actual != value.get("artifact", {}).get("sha256"):
    raise SystemExit("the receipt-bound Fleet Telemetry receiver binary changed")
print(candidate)
PY
)
[ -n "$RECEIVER" ] && [ -f "$RECEIVER" ] && [ ! -L "$RECEIVER" ] || {
  printf '%s\n' 'test-fleet-telemetry-evidence: current Fleet receiver binary is required' >&2
  exit 1
}

python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$RECEIVER" \
  --module-cache "$MODULE_CACHE" \
  --target linux-amd64 \
  --output-dir "$TMP/evidence-a" >/dev/null
python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$RECEIVER" \
  --source-archive "$SOURCE" \
  --module-cache "$MODULE_CACHE" \
  --target linux-amd64 \
  --output-dir "$TMP/evidence-b" >/dev/null

GOPROXY=off GOSUMDB=off GOMODCACHE="$TMP/unavailable-cache" \
  python3 "$SCRIPT" --repo "$REPO" --verify-dir "$TMP/evidence-a"
diff -qr "$TMP/evidence-a" "$TMP/evidence-b"

python3 - "$TMP/evidence-a" "$SCRIPT" <<'PY'
import hashlib
import json
import pathlib
import sys
import tarfile

evidence = pathlib.Path(sys.argv[1])
script = pathlib.Path(sys.argv[2]).resolve()
sys.path.insert(0, str(script.parent))
expected = {
    "FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md",
    "fleet-telemetry-bridge-lock.json",
    "fleet-telemetry-component-manifest.json",
    "fleet-telemetry-dependency-inventory.json",
    "fleet-telemetry-legal-lock.json",
    "fleet-telemetry-license-material.tar.gz",
    "fleet-telemetry-go-module-sources.tar.gz",
    "fleet-telemetry-sbom.spdx.json",
    "fleet-telemetry-upstream-source.tar.gz",
    "fleet-telemetry.unsigned",
}
assert {path.name for path in evidence.iterdir()} == expected
manifest = json.loads((evidence / "fleet-telemetry-component-manifest.json").read_text())
inventory = json.loads((evidence / "fleet-telemetry-dependency-inventory.json").read_text())
legal = json.loads((evidence / "fleet-telemetry-legal-lock.json").read_text())
sbom = json.loads((evidence / "fleet-telemetry-sbom.spdx.json").read_text())
assert manifest["legal_material_complete"] is True
assert manifest["source_material_complete"] is True
assert manifest["runtime_dependency_count"] == 57
assert len(manifest["components"]) == 9
assert inventory["runtime_dependency_count"] == len(inventory["runtime_dependencies"]) == 57
assert sbom["spdxVersion"] == "SPDX-2.3"
assert len(sbom["packages"]) == 58
paho = next(item for item in inventory["runtime_dependencies"] if item["path"] == "github.com/eclipse/paho.mqtt.golang")
assert paho["license_expression"] == "EPL-2.0"
notices = (evidence / "FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md").read_text()
assert "github.com/eclipse/paho.mqtt.golang" in notices
assert "NOTICE.md" in notices
assert "Source availability:" in notices
assert "fleet-telemetry-go-module-sources.tar.gz" in notices
with tarfile.open(evidence / "fleet-telemetry-license-material.tar.gz", "r:gz") as archive:
    actual_material = {member.name for member in archive.getmembers()}
expected_material = {"main/" + item["path"] for item in legal["main"]["license_files"]}
for index, module in enumerate(legal["modules"]):
    expected_material.update(
        f"modules/{index:03d}/{item['path']}" for item in module["license_files"]
    )
assert actual_material == expected_material
with tarfile.open(evidence / "fleet-telemetry-go-module-sources.tar.gz", "r:gz") as archive:
    source_members = {member.name: archive.extractfile(member).read() for member in archive}
assert set(source_members) == {
    f"modules/{index:03d}/{name}"
    for index, _module in enumerate(legal["modules"])
    for name in ("source.zip", "go.mod")
}
paho_index, paho_lock = next(
    (index, item)
    for index, item in enumerate(legal["modules"])
    if item["path"] == "github.com/eclipse/paho.mqtt.golang"
)
paho_source = source_members[f"modules/{paho_index:03d}/source.zip"]
assert hashlib.sha256(paho_source).hexdigest() == paho_lock["zip_sha256"]

import importlib.util
import subprocess
spec = importlib.util.spec_from_file_location("fleet_evidence", script)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
go = subprocess.check_output(["go", "version", "-m", "-json", str(evidence / "fleet-telemetry.unsigned")], text=True)
info = json.loads(go)
module.validate_binary_modules(info, legal)
changed = json.loads(json.dumps(info))
changed["Deps"][0]["Version"] += ".tampered"
try:
    module.validate_binary_modules(changed, legal)
except module.GateError:
    pass
else:
    raise AssertionError("tampered compiled dependency version was accepted")
PY

cp -R "$TMP/evidence-a" "$TMP/notices-tamper"
chmod u+w "$TMP/notices-tamper/FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md"
printf '%s\n' tamper >>"$TMP/notices-tamper/FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md"
expect_failure python3 "$SCRIPT" --repo "$REPO" --verify-dir "$TMP/notices-tamper"

cp -R "$TMP/evidence-a" "$TMP/module-source-tamper"
chmod u+w "$TMP/module-source-tamper/fleet-telemetry-go-module-sources.tar.gz"
printf '%s\n' tamper >>"$TMP/module-source-tamper/fleet-telemetry-go-module-sources.tar.gz"
expect_failure python3 "$SCRIPT" --repo "$REPO" --verify-dir "$TMP/module-source-tamper"

cp -R "$TMP/evidence-a" "$TMP/lock-tamper"
chmod u+w "$TMP/lock-tamper/fleet-telemetry-legal-lock.json"
printf ' ' >>"$TMP/lock-tamper/fleet-telemetry-legal-lock.json"
expect_failure python3 "$SCRIPT" --repo "$REPO" --verify-dir "$TMP/lock-tamper"

cp -R "$TMP/evidence-a" "$TMP/extra-file"
printf '%s\n' extra >"$TMP/extra-file/unreviewed"
expect_failure python3 "$SCRIPT" --repo "$REPO" --verify-dir "$TMP/extra-file"

cp -R "$TMP/evidence-a" "$TMP/symlink-member"
mv "$TMP/symlink-member/fleet-telemetry-sbom.spdx.json" "$TMP/saved-sbom"
ln -s ../saved-sbom "$TMP/symlink-member/fleet-telemetry-sbom.spdx.json"
expect_failure python3 "$SCRIPT" --repo "$REPO" --verify-dir "$TMP/symlink-member"

mkdir "$TMP/empty-cache"
expect_failure python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$RECEIVER" \
  --source-archive "$SOURCE" \
  --module-cache "$TMP/empty-cache" \
  --target linux-amd64 \
  --output-dir "$TMP/missing-modules"

ln -s "$SOURCE" "$TMP/source-link"
expect_failure python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$TMP/receiver" \
  --source-archive "$TMP/source-link" \
  --module-cache "$MODULE_CACHE" \
  --target linux-amd64 \
  --output-dir "$TMP/source-symlink-output"

ln -s "$TMP/receiver" "$TMP/receiver-link"
expect_failure python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$TMP/receiver-link" \
  --source-archive "$SOURCE" \
  --module-cache "$MODULE_CACHE" \
  --target linux-amd64 \
  --output-dir "$TMP/receiver-symlink-output"

ln -s "$MODULE_CACHE" "$TMP/cache-link"
expect_failure python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$TMP/receiver" \
  --source-archive "$SOURCE" \
  --module-cache "$TMP/cache-link" \
  --target linux-amd64 \
  --output-dir "$TMP/cache-symlink-output"

printf '%s\n' 'fleet-telemetry evidence tests passed'
