#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/debian-release-attestation.py"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-debian-attestation-test.XXXXXX")
GPG_HOME=$(mktemp -d /tmp/teslatlas-debian-attestation-gpg.XXXXXX)
cleanup() {
    GNUPGHOME="$GPG_HOME" gpgconf --kill gpg-agent >/dev/null 2>&1 || true
    find "$GPG_HOME" -depth -delete 2>/dev/null || true
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

for command in git gpg gpgconf openssl python3; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "debian attestation test: $command is required" >&2
        exit 69
    }
done

chmod 0700 "$GPG_HOME"
export GNUPGHOME="$GPG_HOME"
printf '%s\n' \
    'Key-Type: RSA' \
    'Key-Length: 2048' \
    'Name-Real: Debian Attestation Fixture' \
    'Name-Email: fixture@example.invalid' \
    '%no-protection' \
    '%commit' >"$TMP/gpg-key.txt"
gpg --batch --generate-key "$TMP/gpg-key.txt" >/dev/null 2>&1
KEY_ID=$(gpg --batch --list-secret-keys --with-colons 2>/dev/null \
    | awk -F: '$1 == "sec" { print $5; exit }')
TAG_FINGERPRINT=$(gpg --batch --fingerprint --with-colons "$KEY_ID" 2>/dev/null \
    | awk -F: '$1 == "fpr" { print $10; exit }')
[ -n "$KEY_ID" ] && [ -n "$TAG_FINGERPRINT" ] || {
    echo 'debian attestation test: cannot create tag-signing fixture key' >&2
    exit 1
}

REPO="$TMP/repo"
mkdir -p "$REPO/scripts" "$REPO/packaging/linux"
git -C "$REPO" init -q
git -C "$REPO" config user.name Fixture
git -C "$REPO" config user.email fixture@example.invalid
git -C "$REPO" config user.signingkey "$KEY_ID"
git -C "$REPO" config gpg.program gpg
cp "$SCRIPT" "$REPO/scripts/debian-release-attestation.py"
chmod 0755 "$REPO/scripts/debian-release-attestation.py"
printf '%s\n' \
    '[package]' \
    'name = "teslatlas-hub"' \
    'version = "1.0.0"' \
    'edition = "2024"' >"$REPO/Cargo.toml"
printf '%s\n' '# fixture lock' >"$REPO/Cargo.lock"
for name in LICENSE NOTICE THIRD_PARTY_NOTICES.md PROVENANCE.md \
    ADDITIONAL_TERMS.md SOURCE_AVAILABILITY.md RELEASE_VERIFICATION.md; do
    printf 'fixture %s\n' "$name" >"$REPO/$name"
done
printf '%s\n' '[Unit]' 'Description=fixture' \
    >"$REPO/packaging/linux/teslatlas-hub.service"
printf '%s\n' '[Unit]' 'Description=failure target fixture' \
    >"$REPO/packaging/linux/teslatlas-hub-terminal-failure.target"
printf '%s\n' 'data_dir = "/var/lib/teslatlas-hub"' \
    >"$REPO/packaging/linux/config.toml"
printf '%s\n' '[Unit]' 'Description=proxy fixture' \
    >"$REPO/packaging/linux/teslatlas-command-proxy.service"
printf '%s\n' '[Unit]' 'Description=receiver fixture' \
    >"$REPO/packaging/linux/teslatlas-fleet-telemetry.service"
printf '%s\n' 'TESLA_HTTP_PROXY_LISTEN=127.0.0.1:4444' \
    >"$REPO/packaging/linux/command-proxy.env"
printf '%s\n' '{}' >"$REPO/packaging/linux/fleet-telemetry.json"
for name in preinst postinst prerm postrm; do
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$REPO/packaging/linux/$name"
    chmod 0755 "$REPO/packaging/linux/$name"
done
python3 - "$REPO/packaging/linux/sidecar-sha256.lock" <<'PY'
from pathlib import Path
import hashlib
import struct
import sys


def fake_elf(machine, marker):
    value = bytearray(64)
    value[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<H", value, 16, 3)
    struct.pack_into("<H", value, 18, machine)
    struct.pack_into("<I", value, 20, 1)
    return bytes(value) + marker


rows = []
for architecture, machine in (("amd64", 62), ("arm64", 183)):
    proxy = fake_elf(machine, b"tesla-http-proxy\n")
    receiver = fake_elf(machine, b"fleet-telemetry\n")
    rows.append(
        f"{architecture} {hashlib.sha256(proxy).hexdigest()} "
        f"{hashlib.sha256(receiver).hexdigest()}"
    )
Path(sys.argv[1]).write_text("\n".join(rows) + "\n")
PY
git -C "$REPO" add .
git -C "$REPO" commit -q -m fixture
git -C "$REPO" tag -s -m fixture v1.0.0
git -C "$REPO" tag -s -m wrong-version v9.9.9

openssl genpkey -algorithm ED25519 -out "$TMP/attestation-private.pem" >/dev/null 2>&1
chmod 0600 "$TMP/attestation-private.pem"
openssl pkey -in "$TMP/attestation-private.pem" -pubout \
    -out "$TMP/attestation-public.pem" >/dev/null 2>&1
openssl genpkey -algorithm ED25519 -out "$TMP/wrong-private.pem" >/dev/null 2>&1
chmod 0600 "$TMP/wrong-private.pem"
openssl pkey -in "$TMP/wrong-private.pem" -pubout \
    -out "$TMP/wrong-public.pem" >/dev/null 2>&1

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}
PUBLIC_DIGEST=$(sha256_file "$TMP/attestation-public.pem")
WRONG_PUBLIC_DIGEST=$(sha256_file "$TMP/wrong-public.pem")

cat >"$TMP/make-fixture.py" <<'PY'
from __future__ import annotations

from io import BytesIO
import hashlib
import json
from pathlib import Path
import struct
import sys
import tarfile


repo = Path(sys.argv[1])
output = Path(sys.argv[2])
variant = sys.argv[3]
binary_path = Path(sys.argv[4]) if len(sys.argv) > 4 else None
native_architecture = sys.argv[5] if len(sys.argv) > 5 else None


def tar_bytes(entries, *, symlink=False, duplicate=False):
    target = BytesIO()
    with tarfile.open(fileobj=target, mode="w:gz") as archive:
        root = tarfile.TarInfo("./")
        root.type = tarfile.DIRTYPE
        root.mode = 0o755
        root.uid = root.gid = 0
        archive.addfile(root)
        for path, data, mode in entries:
            info = tarfile.TarInfo("./" + path)
            info.size = len(data)
            info.mode = mode
            info.mtime = 0
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            archive.addfile(info, BytesIO(data))
        if duplicate:
            path, data, mode = entries[0]
            info = tarfile.TarInfo("./" + path)
            info.size = len(data)
            info.mode = mode
            info.mtime = 0
            info.uid = info.gid = 0
            archive.addfile(info, BytesIO(data))
        if symlink:
            info = tarfile.TarInfo("./usr/bin/unsafe-link")
            info.type = tarfile.SYMTYPE
            info.linkname = "/etc/passwd"
            info.mode = 0o777
            info.uid = info.gid = 0
            archive.addfile(info)
    return target.getvalue()


def ar_member(name, data, *, trailing_slash=True):
    archive_name = name + ("/" if trailing_slash else "")
    header = (
        f"{archive_name:<16}"
        f"{0:<12}"
        f"{0:<6}"
        f"{0:<6}"
        f"{0o100644:<8o}"
        f"{len(data):<10}`\n"
    ).encode("ascii")
    return header + data + (b"\n" if len(data) % 2 else b"")


def fake_elf(machine, marker=b""):
    value = bytearray(64)
    value[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<H", value, 16, 3)
    struct.pack_into("<H", value, 18, machine)
    struct.pack_into("<I", value, 20, 1)
    return bytes(value) + marker


if variant == "malformed-ar":
    output.write_bytes(b"not-an-ar")
    raise SystemExit(0)

version = "9.9.9-1" if variant == "wrong-version" else "1.0.0-1"
architecture = native_architecture or "amd64"
machine = 183 if variant == "wrong-arch" else 62
binary = binary_path.read_bytes() if binary_path else fake_elf(machine)
fleet = variant.startswith("fleet")

control_text = (
    "Package: teslatlas-hub\n"
    f"Version: {version}\n"
    "Section: utils\n"
    "Priority: optional\n"
    f"Architecture: {architecture}\n"
    "Depends: adduser, ca-certificates, systemd (>= 254)\n"
    "Maintainer: György Bolyki <contact@magrathean.uk>\n"
    "Description: Self-hosted multi-car Tesla telemetry hub\n"
    " Fixture package.\n"
)
if variant == "malformed-control":
    control_text += "Package: duplicated\n"
control_entries = [
    ("control", control_text.encode(), 0o644),
    (
        "conffiles",
        b"/etc/teslatlas-hub/config.toml\n"
        + (
            b"/etc/teslatlas-hub/command-proxy.env\n"
            b"/etc/teslatlas-hub/fleet-telemetry.json\n"
            if fleet
            else b""
        ),
        0o644,
    ),
]
for name in ("preinst", "postinst", "prerm", "postrm"):
    control_entries.append((name, (repo / "packaging/linux" / name).read_bytes(), 0o755))

static = {
    "lib/systemd/system/teslatlas-hub.service": "packaging/linux/teslatlas-hub.service",
    "lib/systemd/system/teslatlas-hub-terminal-failure.target":
        "packaging/linux/teslatlas-hub-terminal-failure.target",
    "etc/teslatlas-hub/config.toml": "packaging/linux/config.toml",
    "usr/share/doc/teslatlas-hub/copyright": "LICENSE",
    "usr/share/doc/teslatlas-hub/NOTICE": "NOTICE",
    "usr/share/doc/teslatlas-hub/THIRD_PARTY_NOTICES.md": "THIRD_PARTY_NOTICES.md",
    "usr/share/doc/teslatlas-hub/PROVENANCE.md": "PROVENANCE.md",
    "usr/share/doc/teslatlas-hub/ADDITIONAL_TERMS.md": "ADDITIONAL_TERMS.md",
    "usr/share/doc/teslatlas-hub/SOURCE_AVAILABILITY.md": "SOURCE_AVAILABILITY.md",
    "usr/share/doc/teslatlas-hub/RELEASE_VERIFICATION.md": "RELEASE_VERIFICATION.md",
}
if fleet:
    static.update({
        "lib/systemd/system/teslatlas-command-proxy.service":
            "packaging/linux/teslatlas-command-proxy.service",
        "lib/systemd/system/teslatlas-fleet-telemetry.service":
            "packaging/linux/teslatlas-fleet-telemetry.service",
        "etc/teslatlas-hub/command-proxy.env": "packaging/linux/command-proxy.env",
        "etc/teslatlas-hub/fleet-telemetry.json": "packaging/linux/fleet-telemetry.json",
    })
payload_entries = [("usr/bin/teslatlas-hub", binary, 0o755)]
payload_entries.extend((path, (repo / source).read_bytes(), 0o644) for path, source in static.items())
components = {
    "RUST_THIRD_PARTY_NOTICES.generated.md": b"# fixture notices\n",
    "rust-dependency-inventory.json": b"{}\n",
    "rust-sbom.spdx.json": b"{}\n",
}
if fleet:
    for name in (
        "FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md",
        "GO_THIRD_PARTY_NOTICES.generated.md",
        "fleet-telemetry-bridge-lock.json",
        "fleet-telemetry-dependency-inventory.json",
        "fleet-telemetry-legal-lock.json",
        "fleet-telemetry-license-material.tar.gz",
        "fleet-telemetry-sbom.spdx.json",
        "go-dependency-inventory.json",
        "go-sbom.spdx.json",
    ):
        components[name] = b"{}\n" if name.endswith(".json") else b"fixture material\n"
for name, data in components.items():
    payload_entries.append((f"usr/share/doc/teslatlas-hub/dependency-legal/{name}", data, 0o644))
manifest = {
    "schema": "teslatlas.dependency-legal-bundle/v1",
    "cargo_lock_sha256": hashlib.sha256((repo / "Cargo.lock").read_bytes()).hexdigest(),
    "contains_sidecar_material": fleet,
    "components": [
        {"path": name, "sha256": hashlib.sha256(data).hexdigest(), "size": len(data)}
        for name, data in sorted(components.items())
    ],
}
payload_entries.append((
    "usr/share/doc/teslatlas-hub/dependency-legal/legal-bundle-manifest.json",
    (json.dumps(manifest, sort_keys=True) + "\n").encode(),
    0o644,
))
if fleet:
    proxy = fake_elf(machine, b"tesla-http-proxy\n")
    receiver_marker = (
        b"fleet-telemetry-tampered\n"
        if variant == "fleet-tampered"
        else b"fleet-telemetry\n"
    )
    receiver = fake_elf(machine, receiver_marker)
    sums = (
        f"{hashlib.sha256(proxy).hexdigest()}  tesla-http-proxy\n"
        f"{hashlib.sha256(receiver).hexdigest()}  fleet-telemetry\n"
    ).encode()
    payload_entries.extend([
        ("usr/lib/teslatlas-hub/tesla-http-proxy", proxy, 0o755),
        ("usr/lib/teslatlas-hub/fleet-telemetry", receiver, 0o755),
        ("usr/share/doc/teslatlas-hub/SIDECAR_SHA256SUMS", sums, 0o644),
        (
            "usr/share/doc/teslatlas-hub/SIDECAR_BUILD_LOCK",
            (repo / "packaging/linux/sidecar-sha256.lock").read_bytes(),
            0o644,
        ),
    ])

control_tar = tar_bytes(control_entries)
data_tar = tar_bytes(
    payload_entries,
    symlink=variant == "symlink-member",
    duplicate=variant == "duplicate-tar",
)
parts = [
    ar_member("debian-binary", b"2.0\n", trailing_slash=variant != "good-noslash"),
    ar_member("control.tar.gz", control_tar, trailing_slash=variant != "good-noslash"),
    ar_member("data.tar.gz", data_tar, trailing_slash=variant != "good-noslash"),
]
if variant == "duplicate-ar":
    parts.append(ar_member("data.tar.gz", data_tar))
output.write_bytes(b"!<arch>\n" + b"".join(parts))
PY

for variant in good good-noslash wrong-version wrong-arch symlink-member duplicate-tar \
    duplicate-ar malformed-control malformed-ar fleet fleet-tampered; do
    python3 "$TMP/make-fixture.py" "$REPO" "$TMP/$variant.deb" "$variant"
done
python3 - "$SCRIPT" "$TMP" <<'PY'
from pathlib import Path
import importlib.util
import os
import sys


script = Path(sys.argv[1])
directory = Path(sys.argv[2])
spec = importlib.util.spec_from_file_location("teslatlas_debian_attestation_help", script)
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)

proxy = directory / "tesla-http-proxy"
proxy.write_text(
    "#!/bin/sh\n"
    "[ \"$1\" = --help ] || exit 64\n"
    "printf 'Usage: %s [OPTION...]\\n  --fixture\\n' \"$0\" >&2\n"
)
receiver = directory / "fleet-telemetry"
receiver.write_text(
    "#!/bin/sh\n"
    "[ \"$1\" = --help ] || exit 64\n"
    "printf '2099/01/02 03:04:05 maxprocs: Leaving GOMAXPROCS=77: fixture\\n' >&2\n"
    "printf 'Usage of %s:\\n  -config string\\n' \"$0\" >&2\n"
)
bad = directory / "bad-help"
bad.write_text("#!/bin/sh\nprintf 'unexpected stdout\\n'\n")
for path in (proxy, receiver, bad):
    os.chmod(path, 0o700)

os_release_root = directory / "os-release-root"
etc_directory = os_release_root / "etc"
vendor_directory = os_release_root / "usr" / "lib"
etc_directory.mkdir(parents=True)
vendor_directory.mkdir(parents=True)
etc_release = etc_directory / "os-release"
vendor_release = vendor_directory / "os-release"
contents = b'ID=debian\nVERSION_ID="13"\n'
etc_release.write_bytes(contents)
os.chmod(etc_release, 0o644)
assert module.linux_os_release(
    etc_release,
    vendor_release,
    expected_owner_uid=os.geteuid(),
).data == contents
etc_release.unlink()
vendor_release.write_bytes(contents)
os.chmod(vendor_release, 0o644)
etc_release.symlink_to("../usr/lib/os-release")
assert module.linux_os_release(
    etc_release,
    vendor_release,
    expected_owner_uid=os.geteuid(),
).data == contents
etc_release.unlink()
etc_release.symlink_to("../tmp/untrusted-os-release")
try:
    module.linux_os_release(
        etc_release,
        vendor_release,
        expected_owner_uid=os.geteuid(),
    )
except module.GateError as exc:
    assert "does not resolve to /usr/lib/os-release" in str(exc)
else:
    raise AssertionError("non-canonical os-release symlink was accepted")
etc_release.unlink()
etc_release.symlink_to("../usr/lib/os-release")
os.chmod(vendor_release, 0o666)
try:
    module.linux_os_release(
        etc_release,
        vendor_release,
        expected_owner_uid=os.geteuid(),
    )
except module.GateError as exc:
    assert "not group/world writable" in str(exc)
else:
    raise AssertionError("writable vendor os-release was accepted")
os.chmod(vendor_release, 0o644)
etc_release.unlink()
etc_release.write_bytes(contents)
os.chmod(etc_release, 0o644)
original_read_regular = module.read_regular


def swapping_read_regular(path, label, maximum):
    witness = original_read_regular(path, label, maximum)
    if Path(path) == etc_release:
        replacement = etc_release.with_name("os-release-replacement")
        replacement.write_bytes(contents)
        os.chmod(replacement, 0o644)
        replacement.replace(etc_release)
    return witness


module.read_regular = swapping_read_regular
try:
    module.linux_os_release(
        etc_release,
        vendor_release,
        expected_owner_uid=os.geteuid(),
    )
except module.GateError as exc:
    assert "changed after reading" in str(exc)
else:
    raise AssertionError("replaced os-release path was accepted")
finally:
    module.read_regular = original_read_regular

assert module.normalized_sidecar_help(proxy, "go_proxy", directory) == {
    "arguments": ["--help"],
    "exit_code": 0,
    "stderr": "Usage: tesla-http-proxy [OPTION...]\n  --fixture\n",
    "stdout": "",
}
assert module.normalized_sidecar_help(receiver, "fleet_telemetry", directory) == {
    "arguments": ["--help"],
    "exit_code": 0,
    "stderr": "maxprocs: <runtime>\nUsage of fleet-telemetry:\n  -config string\n",
    "stdout": "",
}
try:
    module.normalized_sidecar_help(bad, "go_proxy", directory)
except module.GateError as exc:
    assert "unexpected stdout" in str(exc)
else:
    raise AssertionError("sidecar help stdout was accepted")
PY
mkdir "$TMP/other"
cp "$TMP/good.deb" "$TMP/other/good.deb"
python3 - "$TMP/other/good.deb" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
data = bytearray(path.read_bytes())
data[24] = ord("1")
path.write_bytes(data)
PY

cat >"$TMP/make-receipt.py" <<'PY'
from __future__ import annotations

import importlib.util
from pathlib import Path
import sys


script, repo, package, signer, output = map(Path, sys.argv[1:6])
spec = importlib.util.spec_from_file_location("teslatlas_debian_attestation_fixture", script)
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
tag = "v1.0.0"
commit, commit_created, fingerprint = module.verify_tag(
    repo, tag, str(signer), require_clean_head=False
)
version = module.cargo_version(repo, tag)
witness = module.read_regular(package, "fixture package", module.MAX_PACKAGE_BYTES)
subject = module.validate_package(repo, tag, version, witness, "amd64")
if subject["contains_fleet_sidecars"]:
    subject["sidecars"]["go_proxy"]["help"] = {
        "arguments": ["--help"],
        "exit_code": 0,
        "stderr": "Usage: tesla-http-proxy [OPTION...]\n  --fixture\n",
        "stdout": "",
    }
    subject["sidecars"]["fleet_telemetry"]["help"] = {
        "arguments": ["--help"],
        "exit_code": 0,
        "stderr": (
            "maxprocs: <runtime>\n"
            "Usage of fleet-telemetry:\n"
            "  -config string\n"
        ),
        "stdout": "",
    }
else:
    assert subject["sidecars"] == {
        "go_proxy": None,
        "fleet_telemetry": None,
    }
source = {
    "cargo_version": version,
    "commit": commit,
    "commit_created_utc": commit_created,
    "tag": tag,
    "tag_signer_fingerprint": fingerprint,
}
host = {
    "debian_architecture": "amd64",
    "kernel_release": "fixture-kernel",
    "machine": "x86_64",
    "os_release_id": "debian",
    "os_release_version_id": "13",
    "sysname": "Linux",
}
toolchain = {
    name: {"executable_sha256": "0" * 64, "version": f"fixture {name}"}
    for name in module.TOOL_COMMANDS
}
receipt = module.make_receipt(
    source=source,
    subject=subject,
    host=host,
    toolchain=toolchain,
    generator=module.expected_generator(repo, tag),
    created_utc="2026-08-28T00:00:00Z",
)
module.validate_receipt_schema(receipt)
output.write_bytes(module.canonical_json(receipt))
PY
python3 "$TMP/make-receipt.py" "$SCRIPT" "$REPO" "$TMP/good.deb" \
    "$TAG_FINGERPRINT" "$TMP/receipt.json"
python3 "$TMP/make-receipt.py" "$SCRIPT" "$REPO" "$TMP/good-noslash.deb" \
    "$TAG_FINGERPRINT" "$TMP/noslash-receipt.json"
python3 "$TMP/make-receipt.py" "$SCRIPT" "$REPO" "$TMP/fleet.deb" \
    "$TAG_FINGERPRINT" "$TMP/fleet-receipt.json"

sign_receipt() {
    receipt=$1
    signature=$2
    key=${3:-$TMP/attestation-private.pem}
    openssl pkeyutl -sign -rawin -inkey "$key" -in "$receipt" \
        -out "$signature" >/dev/null 2>&1
}
sign_receipt "$TMP/receipt.json" "$TMP/receipt.sig"
sign_receipt "$TMP/noslash-receipt.json" "$TMP/noslash-receipt.sig"
sign_receipt "$TMP/fleet-receipt.json" "$TMP/fleet-receipt.sig"

verify_package() {
    package=$1
    architecture=$2
    receipt=$3
    signature=$4
    public_key=$5
    public_digest=$6
    tag=$7
    python3 "$SCRIPT" verify \
        --repo "$REPO" \
        --tag "$tag" \
        --tag-signer-fingerprint "$TAG_FINGERPRINT" \
        --package "$package" \
        --architecture "$architecture" \
        --receipt "$receipt" \
        --signature "$signature" \
        --public-key "$public_key" \
        --public-key-sha256 "$public_digest"
}

expect_fail() {
    label=$1
    pattern=$2
    shift 2
    if "$@" >"$TMP/$label.out" 2>&1; then
        echo "debian attestation test: $label was accepted" >&2
        exit 1
    fi
    grep -Fq "$pattern" "$TMP/$label.out" || {
        echo "debian attestation test: $label failed for the wrong reason" >&2
        cat "$TMP/$label.out" >&2
        exit 1
    }
}

verify_package "$TMP/good.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0 >"$TMP/good.out"
grep -Fq 'Debian native attestation verified:' "$TMP/good.out"
verify_package "$TMP/good-noslash.deb" amd64 \
    "$TMP/noslash-receipt.json" "$TMP/noslash-receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0 \
    >"$TMP/good-noslash.out"
grep -Fq 'Debian native attestation verified:' "$TMP/good-noslash.out"
verify_package "$TMP/fleet.deb" amd64 \
    "$TMP/fleet-receipt.json" "$TMP/fleet-receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0 >"$TMP/fleet.out"
grep -Fq 'Debian native attestation verified:' "$TMP/fleet.out"

expect_fail wrong-pin 'does not match the pinned SHA-256 trust anchor' \
    verify_package "$TMP/good.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$(printf '%064d' 0)" v1.0.0
expect_fail wrong-key 'verify Debian native attestation signature failed' \
    verify_package "$TMP/good.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/wrong-public.pem" "$WRONG_PUBLIC_DIGEST" v1.0.0

cp "$TMP/receipt.json" "$TMP/tampered.json"
printf ' ' >>"$TMP/tampered.json"
expect_fail tampered 'verify Debian native attestation signature failed' \
    verify_package "$TMP/good.deb" amd64 "$TMP/tampered.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0

python3 - "$TMP/receipt.json" "$TMP/wrong-source.json" <<'PY'
import json
from pathlib import Path
import sys
source, output = map(Path, sys.argv[1:3])
value = json.loads(source.read_text())
value["source"]["cargo_version"] = "9.9.9"
output.write_text(json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True) + "\n")
PY
sign_receipt "$TMP/wrong-source.json" "$TMP/wrong-source.sig"
expect_fail wrong-source 'attestation source does not match the signed tag' \
    verify_package "$TMP/good.deb" amd64 "$TMP/wrong-source.json" "$TMP/wrong-source.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0

python3 - "$TMP/receipt.json" "$TMP/base-sidecar.json" \
    "$TMP/fleet-receipt.json" "$TMP/sidecar-extra.json" \
    "$TMP/sidecar-help.json" "$TMP/sidecar-sha.json" <<'PY'
import json
from pathlib import Path
import sys


base_source, base_output, fleet_source, extra_output, help_output, sha_output = map(
    Path, sys.argv[1:7]
)
base = json.loads(base_source.read_text())
base["subject"]["sidecars"]["go_proxy"] = {"forged": True}
base_output.write_text(json.dumps(base, ensure_ascii=True, separators=(",", ":"), sort_keys=True) + "\n")

fleet = json.loads(fleet_source.read_text())
extra = json.loads(fleet_source.read_text())
extra["subject"]["sidecars"]["go_proxy"]["unexpected"] = True
extra_output.write_text(json.dumps(extra, ensure_ascii=True, separators=(",", ":"), sort_keys=True) + "\n")

bad_help = json.loads(fleet_source.read_text())
bad_help["subject"]["sidecars"]["fleet_telemetry"]["help"]["stderr"] = "Usage of fleet-telemetry:\n"
help_output.write_text(json.dumps(bad_help, ensure_ascii=True, separators=(",", ":"), sort_keys=True) + "\n")

bad_sha = json.loads(fleet_source.read_text())
bad_sha["subject"]["sidecars"]["go_proxy"]["sha256"] = "0" * 64
sha_output.write_text(json.dumps(bad_sha, ensure_ascii=True, separators=(",", ":"), sort_keys=True) + "\n")
PY
for name in base-sidecar sidecar-extra sidecar-help sidecar-sha; do
    sign_receipt "$TMP/$name.json" "$TMP/$name.sig"
done
expect_fail base-sidecar 'base-package attestation must explicitly contain no sidecars' \
    verify_package "$TMP/good.deb" amd64 "$TMP/base-sidecar.json" "$TMP/base-sidecar.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail sidecar-extra 'sidecar go_proxy has an unexpected schema shape' \
    verify_package "$TMP/fleet.deb" amd64 "$TMP/sidecar-extra.json" "$TMP/sidecar-extra.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail sidecar-help 'sidecar fleet_telemetry help usage is invalid' \
    verify_package "$TMP/fleet.deb" amd64 "$TMP/sidecar-help.json" "$TMP/sidecar-help.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail sidecar-sha 'attestation subject does not match the supplied Debian package' \
    verify_package "$TMP/fleet.deb" amd64 "$TMP/sidecar-sha.json" "$TMP/sidecar-sha.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail fleet-package-bytes 'packaged sidecars do not match the signed architecture lock' \
    verify_package "$TMP/fleet-tampered.deb" amd64 \
    "$TMP/fleet-receipt.json" "$TMP/fleet-receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0

expect_fail wrong-package 'attestation subject does not match the supplied Debian package' \
    verify_package "$TMP/other/good.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail wrong-version 'Debian package version does not match Cargo version' \
    verify_package "$TMP/wrong-version.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail wrong-tag 'release tag does not match Cargo version; expected v1.0.0' \
    verify_package "$TMP/good.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v9.9.9
expect_fail wrong-architecture 'does not match the expected architecture' \
    verify_package "$TMP/good.deb" arm64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail wrong-elf-architecture 'architecture does not match Debian control metadata' \
    verify_package "$TMP/wrong-arch.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0

ln -s "$TMP/good.deb" "$TMP/package-link.deb"
expect_fail symlink-input 'bounded regular non-symlink file' \
    verify_package "$TMP/package-link.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
cp "$TMP/good.deb" "$TMP/package-hardlink-source.deb"
ln "$TMP/package-hardlink-source.deb" "$TMP/package-hardlink.deb"
expect_fail hardlink-input 'bounded regular non-symlink file' \
    verify_package "$TMP/package-hardlink.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail symlink-member 'contains a non-regular member' \
    verify_package "$TMP/symlink-member.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail duplicate-tar 'contains a duplicate path' \
    verify_package "$TMP/duplicate-tar.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail duplicate-ar 'contains invalid duplicate members' \
    verify_package "$TMP/duplicate-ar.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail malformed-control 'invalid or duplicate field' \
    verify_package "$TMP/malformed-control.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
expect_fail malformed-ar 'is not an ar archive' \
    verify_package "$TMP/malformed-ar.deb" amd64 "$TMP/receipt.json" "$TMP/receipt.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0

python3 - "$TMP/receipt.json" "$TMP/duplicate-json.json" <<'PY'
from pathlib import Path
import sys
source, output = map(Path, sys.argv[1:3])
data = source.read_text()
output.write_text('{"schema":"duplicate",' + data[1:])
PY
sign_receipt "$TMP/duplicate-json.json" "$TMP/duplicate-json.sig"
expect_fail duplicate-json 'duplicate JSON key: schema' \
    verify_package "$TMP/good.deb" amd64 "$TMP/duplicate-json.json" "$TMP/duplicate-json.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0
printf '{' >"$TMP/malformed.json"
sign_receipt "$TMP/malformed.json" "$TMP/malformed.sig"
expect_fail malformed-json 'attestation receipt is invalid JSON' \
    verify_package "$TMP/good.deb" amd64 "$TMP/malformed.json" "$TMP/malformed.sig" \
    "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0

if [ "$(uname -s)" = Linux ] \
    && [ -r /etc/os-release ] \
    && grep -Eq '^ID=("?debian"?)$' /etc/os-release \
    && grep -Eq '^VERSION_ID=("?13"?)$' /etc/os-release \
    && command -v rustc >/dev/null 2>&1 \
    && command -v cargo >/dev/null 2>&1 \
    && command -v cc >/dev/null 2>&1 \
    && command -v dpkg >/dev/null 2>&1 \
    && command -v dpkg-deb >/dev/null 2>&1 \
    && command -v readelf >/dev/null 2>&1; then
    case "$(dpkg --print-architecture)" in
        amd64|arm64) NATIVE_ARCHITECTURE=$(dpkg --print-architecture) ;;
        *) NATIVE_ARCHITECTURE= ;;
    esac
    if [ -n "$NATIVE_ARCHITECTURE" ]; then
        cat >"$TMP/native.rs" <<'RS'
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("teslatlas-hub 1.0.0");
    } else {
        std::process::exit(64);
    }
}
RS
        rustc "$TMP/native.rs" -o "$TMP/native-hub"
        python3 "$TMP/make-fixture.py" "$REPO" "$TMP/native.deb" good \
            "$TMP/native-hub" "$NATIVE_ARCHITECTURE"
        python3 "$SCRIPT" generate \
            --repo "$REPO" \
            --tag v1.0.0 \
            --tag-signer-fingerprint "$TAG_FINGERPRINT" \
            --package "$TMP/native.deb" \
            --architecture "$NATIVE_ARCHITECTURE" \
            --signing-key "$TMP/attestation-private.pem" \
            --output-dir "$TMP/native-attestation"
        verify_package "$TMP/native.deb" "$NATIVE_ARCHITECTURE" \
            "$TMP/native-attestation/debian-native-attestation.json" \
            "$TMP/native-attestation/debian-native-attestation.sig" \
            "$TMP/attestation-public.pem" "$PUBLIC_DIGEST" v1.0.0 \
            >"$TMP/native-verify.out"
        grep -Fq 'Debian native attestation verified:' "$TMP/native-verify.out"
    fi
else
    expect_fail non-linux-generation 'attestation generation requires native Linux' \
        python3 "$SCRIPT" generate \
        --repo "$REPO" \
        --tag v1.0.0 \
        --tag-signer-fingerprint "$TAG_FINGERPRINT" \
        --package "$TMP/good.deb" \
        --architecture amd64 \
        --signing-key "$TMP/attestation-private.pem" \
        --output-dir "$TMP/forbidden-generation"
fi

printf '%s\n' 'Debian native release attestation tests passed'
