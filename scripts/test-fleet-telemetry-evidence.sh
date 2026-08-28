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
import hashlib
import pathlib
import sys

repo = pathlib.Path(sys.argv[1])
lock = json.loads(
    (repo / "packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json").read_text()
)
upstream = lock["upstream"]
print(
    repo
    / "target"
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
printf '%s\n' 'deterministic-test-receiver' >"$TMP/receiver"

python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$TMP/receiver" \
  --source-archive "$SOURCE" \
  --module-cache "$MODULE_CACHE" \
  --target linux-amd64 \
  --output-dir "$TMP/evidence-a" >/dev/null
python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$TMP/receiver" \
  --source-archive "$SOURCE" \
  --module-cache "$MODULE_CACHE" \
  --target linux-amd64 \
  --output-dir "$TMP/evidence-b" >/dev/null

GOPROXY=off GOSUMDB=off GOMODCACHE="$TMP/unavailable-cache" \
  python3 "$SCRIPT" --repo "$REPO" --verify-dir "$TMP/evidence-a"
diff -qr "$TMP/evidence-a" "$TMP/evidence-b"

python3 - "$TMP/evidence-a" <<'PY'
import hashlib
import json
import pathlib
import sys
import tarfile

evidence = pathlib.Path(sys.argv[1])
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
assert manifest["runtime_dependency_count"] == 45
assert len(manifest["components"]) == 9
assert inventory["runtime_dependency_count"] == len(inventory["runtime_dependencies"]) == 45
assert sbom["spdxVersion"] == "SPDX-2.3"
assert len(sbom["packages"]) == 46
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
  --receiver-binary "$TMP/receiver" \
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
