#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Builds the fleet-telemetry receiver for darwin-arm64 in scratch from the pinned,
# cached upstream archive under TESLATLAS_LAB, offline, then checks its evidence.
# TESLATLAS_FLEET_RECEIVER (a regular file built for darwin-arm64) skips the build;
# the cached archive is still required. The build steps copy those of
# scripts/build-fleet-telemetry-bridge.sh and must stay in step with it.
set -eu

# Keep toolchain selection, receiver compilation and evidence generation offline.
GOENV=off
GOWORK=off
GOTOOLCHAIN=local
GOFLAGS=-mod=readonly
GOPROXY=off
GOSUMDB=off
GONOPROXY=none
PYTHONDONTWRITEBYTECODE=1
export GOENV GOWORK GOTOOLCHAIN GOFLAGS GOPROXY GOSUMDB GONOPROXY PYTHONDONTWRITEBYTECODE

die() {
  printf '%s\n' "test-fleet-telemetry-evidence: $*" >&2
  exit 1
}

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
GO=$(python3 "$REPO/scripts/go_toolchain.py" --expected-version go1.27.3) \
  || die 'Go 1.27.3 is required for offline Fleet Telemetry evidence'
TESLATLAS_GO=$GO
export TESLATLAS_GO
MODULE_CACHE=$("$GO" env GOMODCACHE) || die 'cannot locate the offline Go module cache'
if [ -n "${TESLATLAS_FLEET_RECEIVER:-}" ]; then
  [ -f "$TESLATLAS_FLEET_RECEIVER" ] && [ ! -L "$TESLATLAS_FLEET_RECEIVER" ] \
    || die 'TESLATLAS_FLEET_RECEIVER must name a regular receiver binary, not a symlink'
  cp "$TESLATLAS_FLEET_RECEIVER" "$TMP/receiver"
else
  # Use the builder's pinned archive, patch and module graph without its downloads.
  SOURCE_DIRECTORY=$(python3 - "$REPO" "$SOURCE" "$TMP" <<'PY'
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys
import tarfile

repo, archive, scratch = map(pathlib.Path, sys.argv[1:])
packaging = repo / "packaging/fleet-telemetry-bridge"
lock = json.loads((packaging / "fleet-telemetry-bridge-lock.json").read_text())
upstream, overlay = lock["upstream"], lock["overlay"]
graph = overlay["module_graph"]
patch = packaging / overlay["patch"]
go_mod, go_sum = (packaging / graph[key] for key in ("go_mod", "go_sum"))
for path, digest in (
    (archive, upstream["archive_sha256"]),
    (patch, overlay["patch_sha256"]),
    (go_mod, graph["go_mod_sha256"]),
    (go_sum, graph["go_sum_sha256"]),
):
    if not path.is_file() or path.is_symlink():
        raise SystemExit(f"required offline build input is missing or unsafe: {path}")
    if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise SystemExit(f"offline build input checksum mismatch: {path}")
root = "fleet-telemetry-" + upstream["commit"]
with tarfile.open(archive, "r:gz") as source:
    for member in source.getmembers():
        name = pathlib.PurePosixPath(member.name)
        if (name.is_absolute() or ".." in name.parts or not name.parts
                or name.parts[0] != root or not (member.isdir() or member.isreg())):
            raise SystemExit("pinned source archive has an unsafe member")
    source.extractall(scratch)
directory = scratch / root
with patch.open("rb") as patch_input:
    subprocess.run(["patch", "-p1", "--batch", "--forward"], cwd=directory,
                   stdin=patch_input, stdout=sys.stderr, check=True)
shutil.copyfile(go_mod, directory / "go.mod")
shutil.copyfile(go_sum, directory / "go.sum")
print(directory)
PY
  ) || die 'cannot prepare the offline receiver build; populate the pinned source cache or set TESLATLAS_FLEET_RECEIVER'
  (
    cd "$SOURCE_DIRECTORY" || exit 1
    GOCACHE="$TMP/go-build-cache" CGO_ENABLED=0 "$GO" test ./datastore/teslatlas &&
    # Match the reviewed Darwin dependency closure in the evidence lock.
    GOCACHE="$TMP/go-build-cache" CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 GOARM64=v8.0 \
      "$GO" build -trimpath -buildvcs=false -ldflags='-s -w' -o "$TMP/receiver" ./cmd
  ) || die 'cannot build the receiver offline; populate the pinned Go module cache or set TESLATLAS_FLEET_RECEIVER'
fi
RECEIVER="$TMP/receiver"
[ -n "$RECEIVER" ] && [ -f "$RECEIVER" ] && [ ! -L "$RECEIVER" ] || {
  die 'a Fleet Telemetry receiver binary is required'
}

python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$RECEIVER" \
  --module-cache "$MODULE_CACHE" \
  --target darwin-arm64 \
  --output-dir "$TMP/evidence-a" >/dev/null
python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$RECEIVER" \
  --source-archive "$SOURCE" \
  --module-cache "$MODULE_CACHE" \
  --target darwin-arm64 \
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
import os
go = subprocess.check_output([os.environ["TESLATLAS_GO"], "version", "-m", "-json", str(evidence / "fleet-telemetry.unsigned")], text=True)
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
  --target darwin-arm64 \
  --output-dir "$TMP/missing-modules"

ln -s "$SOURCE" "$TMP/source-link"
expect_failure python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$TMP/receiver" \
  --source-archive "$TMP/source-link" \
  --module-cache "$MODULE_CACHE" \
  --target darwin-arm64 \
  --output-dir "$TMP/source-symlink-output"

ln -s "$TMP/receiver" "$TMP/receiver-link"
expect_failure python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$TMP/receiver-link" \
  --source-archive "$SOURCE" \
  --module-cache "$MODULE_CACHE" \
  --target darwin-arm64 \
  --output-dir "$TMP/receiver-symlink-output"

ln -s "$MODULE_CACHE" "$TMP/cache-link"
expect_failure python3 "$SCRIPT" \
  --repo "$REPO" \
  --receiver-binary "$TMP/receiver" \
  --source-archive "$SOURCE" \
  --module-cache "$TMP/cache-link" \
  --target darwin-arm64 \
  --output-dir "$TMP/cache-symlink-output"

printf '%s\n' 'fleet-telemetry evidence tests passed'
