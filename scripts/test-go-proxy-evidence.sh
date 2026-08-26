#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
HELPER="$ROOT/scripts/go-proxy-evidence.py"
LOCK="$ROOT/scripts/tesla-proxy-lock.json"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-go-proxy-evidence-test.XXXXXX")
cleanup() {
    chmod -R u+w "$TMP" 2>/dev/null || true
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$TMP/build-cache"
GOCACHE="$TMP/build-cache" \
    "$ROOT/scripts/build-tesla-command-proxy.sh" --output "$TMP/tesla-http-proxy" \
    >/dev/null

python3 "$HELPER" --repo "$ROOT" --proxy-binary "$TMP/tesla-http-proxy" \
    --output-dir "$TMP/evidence-one" >/dev/null
python3 "$HELPER" --repo "$ROOT" --proxy-binary "$TMP/tesla-http-proxy" \
    --output-dir "$TMP/evidence-two" >/dev/null
python3 "$HELPER" --repo "$ROOT" --verify-dir "$TMP/evidence-one" >/dev/null

for name in \
    GO_THIRD_PARTY_NOTICES.generated.md \
    go-build-receipt.json \
    go-component-manifest.json \
    go-dependency-inventory.json \
    go-sbom.spdx.json \
    tesla-http-proxy.unsigned \
    tesla-http-proxy-go-sources.tar.gz
do
    test -s "$TMP/evidence-one/$name"
    cmp "$TMP/evidence-one/$name" "$TMP/evidence-two/$name"
done

python3 - "$TMP/evidence-one" "$TMP/tesla-http-proxy" <<'PY'
import gzip
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile

evidence = Path(sys.argv[1])
proxy = Path(sys.argv[2])
digest = lambda data: hashlib.sha256(data).hexdigest()

manifest = json.loads((evidence / "go-component-manifest.json").read_text())
assert manifest["schema"] == "teslatlas.go-proxy-evidence/v1"
assert manifest["source_module_count"] == 19
assert manifest["runtime_dependency_count"] == 18
assert manifest["clean_rebuild_byte_identical"] is True
assert manifest["subject"]["sha256"] == digest(proxy.read_bytes())
expected_components = {
    "GO_THIRD_PARTY_NOTICES.generated.md",
    "go-build-receipt.json",
    "go-dependency-inventory.json",
    "go-sbom.spdx.json",
    "tesla-http-proxy.unsigned",
    "tesla-http-proxy-go-sources.tar.gz",
}
assert {item["path"] for item in manifest["components"]} == expected_components
for item in manifest["components"]:
    data = (evidence / item["path"]).read_bytes()
    assert len(data) == item["size"]
    assert digest(data) == item["sha256"]
assert (evidence / "tesla-http-proxy.unsigned").read_bytes() == proxy.read_bytes()

inventory = json.loads((evidence / "go-dependency-inventory.json").read_text())
assert inventory["runtime_dependency_count"] == 18
assert len(inventory["runtime_dependencies"]) == 18
replacement = next(
    item for item in inventory["runtime_dependencies"]
    if item["path"] == "github.com/JuulLabs-OSS/cbgo"
)
assert replacement["replacement"]["path"] == "github.com/tinygo-org/cbgo"

sbom = json.loads((evidence / "go-sbom.spdx.json").read_text())
assert sbom["spdxVersion"] == "SPDX-2.3"
assert len(sbom["packages"]) == 19
assert all(package["licenseDeclared"] != "NOASSERTION" for package in sbom["packages"])

receipt = json.loads((evidence / "go-build-receipt.json").read_text())
assert receipt["clean_rebuild_byte_identical"] is True
assert receipt["clean_rebuild_sha256"] == digest(proxy.read_bytes())
assert receipt["strict_go_environment"]["GOFLAGS"] == ""
assert receipt["toolchain"]["go_version"] == "go1.27.0"
assert receipt["toolchain"]["godebug_default"] == "go1.27"
assert "DefaultGODEBUG" not in {
    item["Key"] for item in receipt["build_info"]["Settings"]
}
assert len(receipt["build_host"]["go"]["sha256"]) == 64
assert len(receipt["build_host"]["compiler"]["sha256"]) == 64
assert receipt["build_host"]["go"]["goroot"]
assert receipt["build_host"]["xcode"]["version"].startswith("Xcode ")
assert receipt["build_host"]["sdk"]["version"] == "27.0"
assert receipt["source_configuration"]["archived_upstream_source_unchanged"] is True
assert receipt["source_configuration"]["private_build_copy_go_mod_directive"] == "godebug default=go1.27"

notice = (evidence / "GO_THIRD_PARTY_NOTICES.generated.md").read_text()
assert notice.count("----- BEGIN EXACT LICENSE TEXT -----") == 20
assert "github.com/tinygo-org/cbgo@v0.0.4" in notice

archive_data = gzip.decompress((evidence / "tesla-http-proxy-go-sources.tar.gz").read_bytes())
with tarfile.open(fileobj=io.BytesIO(archive_data), mode="r:") as archive:
    members = archive.getmembers()
    names = [member.name for member in members]
    assert all(member.isfile() and not member.issym() and member.mtime == 0 for member in members)
    assert len([name for name in names if name.endswith("/module.zip")]) == 19
    assert len([name for name in names if name.endswith("/module.mod")]) == 19
    assert len([name for name in names if name.endswith("/source.json")]) == 19
    assert "tesla-http-proxy-go-sources/tesla-proxy-lock.json" in names
    for index in range(19):
        root = f"tesla-http-proxy-go-sources/modules/{index:02d}"
        source = json.load(archive.extractfile(f"{root}/source.json"))
        assert digest(archive.extractfile(f"{root}/module.zip").read()) == source["zip_sha256"]
        assert digest(archive.extractfile(f"{root}/module.mod").read()) == source["go_mod_sha256"]
PY

cp -R "$TMP/evidence-one" "$TMP/mutated-published-evidence"
printf '%s\n' tamper >>"$TMP/mutated-published-evidence/go-build-receipt.json"
if python3 "$HELPER" --repo "$ROOT" \
    --verify-dir "$TMP/mutated-published-evidence" \
    >"$TMP/mutated-published.out" 2>&1; then
    echo 'go-proxy-evidence test: mutated published evidence was accepted' >&2
    exit 1
fi
grep -Fq 'does not match its manifest' "$TMP/mutated-published.out"

python3 - "$TMP/evidence-one" "$TMP" <<'PY'
import hashlib
import json
from pathlib import Path
import shutil
import sys

source = Path(sys.argv[1])
root = Path(sys.argv[2])

def rewrite_manifest(evidence):
    manifest_path = evidence / "go-component-manifest.json"
    manifest = json.loads(manifest_path.read_text())
    for component in manifest["components"]:
        data = (evidence / component["path"]).read_bytes()
        component["sha256"] = hashlib.sha256(data).hexdigest()
        component["size"] = len(data)
    component_set = "".join(
        f"{item['sha256']}  {item['path']}\n"
        for item in sorted(manifest["components"], key=lambda item: item["path"])
    ).encode()
    manifest["component_set_sha256"] = hashlib.sha256(component_set).hexdigest()
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    )

def json_forgery(name, component, mutate):
    evidence = root / name
    shutil.copytree(source, evidence)
    path = evidence / component
    value = json.loads(path.read_text())
    mutate(value)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    rewrite_manifest(evidence)

json_forgery(
    "forged-receipt", "go-build-receipt.json",
    lambda value: value.__setitem__("clean_rebuild_sha256", "0" * 64),
)
json_forgery(
    "forged-build-host", "go-build-receipt.json",
    lambda value: value["build_host"]["xcode"].__setitem__("build", "forged"),
)
json_forgery(
    "forged-inventory", "go-dependency-inventory.json",
    lambda value: value.__setitem__("runtime_dependency_count", 17),
)
json_forgery(
    "forged-sbom", "go-sbom.spdx.json",
    lambda value: value["packages"][0].__setitem__("name", "forged/module"),
)

notices = root / "forged-notices"
shutil.copytree(source, notices)
with (notices / "GO_THIRD_PARTY_NOTICES.generated.md").open("ab") as output:
    output.write(b"forged notice\n")
rewrite_manifest(notices)

archive = root / "forged-archive"
shutil.copytree(source, archive)
with (archive / "tesla-http-proxy-go-sources.tar.gz").open("ab") as output:
    output.write(b"forged archive\n")
rewrite_manifest(archive)

proxy = root / "forged-proxy-component"
shutil.copytree(source, proxy)
with (proxy / "tesla-http-proxy.unsigned").open("ab") as output:
    output.write(b"forged proxy\n")
rewrite_manifest(proxy)
PY
for forgery in forged-receipt forged-build-host forged-inventory forged-sbom \
    forged-notices forged-archive forged-proxy-component
do
    if python3 "$HELPER" --repo "$ROOT" --verify-dir "$TMP/$forgery" \
        >"$TMP/$forgery.out" 2>&1; then
        echo "go-proxy-evidence test: $forgery was accepted" >&2
        exit 1
    fi
done
grep -Fq 'does not prove the locked reproducible subject' "$TMP/forged-receipt.out"
grep -Fq 'host identity does not match the exact lock' "$TMP/forged-build-host.out"
grep -Fq 'inventory does not match the exact lock' "$TMP/forged-inventory.out"
grep -Fq 'SBOM does not match the locked source archive' "$TMP/forged-sbom.out"
grep -Fq 'notices do not match the locked source licenses' "$TMP/forged-notices.out"
grep -Eq 'valid gzip stream|canonical reproducible format' "$TMP/forged-archive.out"
grep -Fq 'unsigned Tesla proxy component does not match the locked subject' \
    "$TMP/forged-proxy-component.out"

cp "$TMP/tesla-http-proxy" "$TMP/tampered-proxy"
printf '%s\n' tamper >>"$TMP/tampered-proxy"
if python3 "$HELPER" --repo "$ROOT" --proxy-binary "$TMP/tampered-proxy" \
    --output-dir "$TMP/tampered-evidence" >"$TMP/tampered.out" 2>&1; then
    echo 'go-proxy-evidence test: tampered binary was accepted' >&2
    exit 1
fi
test ! -e "$TMP/tampered-evidence"

mkdir -p "$TMP/wrong-repo/scripts"
sed 's/"go_version": "go1.27.0"/"go_version": "go1.26.0"/' "$LOCK" \
    >"$TMP/wrong-repo/scripts/tesla-proxy-lock.json"
if python3 "$HELPER" --repo "$TMP/wrong-repo" --proxy-binary "$TMP/tesla-http-proxy" \
    --output-dir "$TMP/wrong-toolchain-evidence" >"$TMP/wrong-toolchain.out" 2>&1; then
    echo 'go-proxy-evidence test: wrong toolchain lock was accepted' >&2
    exit 1
fi
grep -Fq 'reviewed build policy' "$TMP/wrong-toolchain.out"
test ! -e "$TMP/wrong-toolchain-evidence"

ln -s "$TMP/tesla-http-proxy" "$TMP/proxy-link"
if python3 "$HELPER" --repo "$ROOT" --proxy-binary "$TMP/proxy-link" \
    --output-dir "$TMP/symlink-evidence" >"$TMP/symlink.out" 2>&1; then
    echo 'go-proxy-evidence test: symlink proxy was accepted' >&2
    exit 1
fi
grep -Fq 'regular, non-symlink file' "$TMP/symlink.out"
test ! -e "$TMP/symlink-evidence"

mkdir "$TMP/existing-evidence"
printf '%s\n' keep >"$TMP/existing-evidence/marker"
if python3 "$HELPER" --repo "$ROOT" --proxy-binary "$TMP/tesla-http-proxy" \
    --output-dir "$TMP/existing-evidence" >"$TMP/existing.out" 2>&1; then
    echo 'go-proxy-evidence test: existing output was replaced' >&2
    exit 1
fi
test "$(cat "$TMP/existing-evidence/marker")" = keep

printf '%s\n' 'go-proxy-evidence fail-closed test passed'
