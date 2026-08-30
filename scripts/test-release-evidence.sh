#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/release-evidence.py"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-release-evidence-test.XXXXXX")
GPG_TMP=

if [ -x /usr/bin/python3 ]; then
    PYTHONDONTWRITEBYTECODE=1 /usr/bin/python3 - "$SCRIPT" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("release_evidence_python39", sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
assert module.cargo_package_version
assert module.cargo_lock_checksums
PY
fi

cleanup() {
    if [ -n "$GPG_TMP" ] && [ -d "$GPG_TMP" ]; then
        GNUPGHOME="$GPG_TMP" gpgconf --kill gpg-agent >/dev/null 2>&1 || true
        find "$GPG_TMP" -depth -delete 2>/dev/null || true
    fi
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

assert_flat_files() {
    directory=$1
    shift
    python3 - "$directory" "$@" <<'PY'
from pathlib import Path
import sys

directory = Path(sys.argv[1])
expected = set(sys.argv[2:])
actual = {entry.name for entry in directory.iterdir()}
assert actual == expected, (actual, expected)
assert all(entry.is_file() and not entry.is_symlink() for entry in directory.iterdir())
PY
}

mkdir -p "$TMP/repo"
git -C "$TMP/repo" init -q
git -C "$TMP/repo" config user.name Test
git -C "$TMP/repo" config user.email test@example.invalid
printf '%s\n' '[package]' 'name = "fixture"' 'version = "1.0.0"' 'edition = "2024"' \
    >"$TMP/repo/Cargo.toml"
printf '%s\n' 'fn main() {}' >"$TMP/repo/main.rs"
printf '%s\n' 'fixture release key' >"$TMP/repo/RELEASE_SIGNING_KEY.asc"
printf '%s\n' artifact >"$TMP/repo/artifact.bin"
git -C "$TMP/repo" add Cargo.toml main.rs RELEASE_SIGNING_KEY.asc
git -C "$TMP/repo" commit -q -m fixture
git -C "$TMP/repo" tag -a -m fixture v1.0.0

if python3 "$SCRIPT" --repo "$TMP/repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$(printf '%064d' 0)" \
    --output-dir "$TMP/repo/evidence" --signing-key "$TMP/missing.pem" \
    --public-key-sha256 "$(printf '%064d' 0)" \
    --artifact "$TMP/repo/artifact.bin" --legal-bundle "$TMP/missing-legal" \
    --rust-source-evidence "$TMP/missing-rust-source" \
    >"$TMP/out" 2>&1; then
    echo 'release-evidence test: missing signing key was accepted' >&2
    exit 1
fi
grep -Fq 'regular, non-symlink file' "$TMP/out"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
    -out "$TMP/signing.pem" >/dev/null 2>&1
git -C "$TMP/repo" tag -a -m wrong-version v9.9.9
if python3 "$SCRIPT" --repo "$TMP/repo" --tag v9.9.9 \
    --tag-signer-fingerprint "$(printf '%064d' 0)" \
    --output-dir "$TMP/wrong-version" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$(printf '%064d' 0)" \
    --artifact "$TMP/repo/artifact.bin" --legal-bundle "$TMP/missing-legal" \
    --rust-source-evidence "$TMP/missing-rust-source" \
    >"$TMP/wrong-version.out" 2>&1; then
    echo 'release-evidence test: tag/Cargo version mismatch was accepted' >&2
    exit 1
fi
grep -Fq 'release tag does not match Cargo package version; expected v1.0.0' \
    "$TMP/wrong-version.out"
if python3 "$SCRIPT" --repo "$TMP/repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$(printf '%064d' 0)" \
    --output-dir "$TMP/retry" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$(printf '%064d' 0)" \
    --artifact "$TMP/repo/artifact.bin" --legal-bundle "$TMP/missing-legal" \
    --rust-source-evidence "$TMP/missing-rust-source" \
    >"$TMP/retry.out" 2>&1; then
    echo 'release-evidence test: unsigned tag was accepted' >&2
    exit 1
fi
grep -Fq 'Rust source evidence must be a real directory' "$TMP/retry.out"

command -v gpg >/dev/null 2>&1
GPG_TMP=$(mktemp -d /tmp/tlh-gpg.XXXXXX)
chmod 700 "$GPG_TMP"
printf '%s\n' \
    'Key-Type: RSA' 'Key-Length: 2048' 'Name-Real: Fixture' \
    'Name-Email: fixture@example.invalid' '%no-protection' '%commit' \
    >"$TMP/gpg-key.txt"
GNUPGHOME="$GPG_TMP" gpg --batch --generate-key "$TMP/gpg-key.txt" >/dev/null 2>&1
KEYID=$(GNUPGHOME="$GPG_TMP" gpg --batch --list-secret-keys --with-colons \
    2>/dev/null | awk -F: '$1 == "sec" { print $5; exit }')
TAG_FINGERPRINT=$(GNUPGHOME="$GPG_TMP" gpg --batch --fingerprint --with-colons "$KEYID" \
    2>/dev/null | awk -F: '$1 == "fpr" { print $10; exit }')
mkdir "$TMP/signed-repo"
git -C "$TMP/signed-repo" init -q
git -C "$TMP/signed-repo" config user.name Fixture
git -C "$TMP/signed-repo" config user.email fixture@example.invalid
git -C "$TMP/signed-repo" config user.signingkey "$KEYID"
git -C "$TMP/signed-repo" config gpg.program gpg
printf '%s\n' \
    '[package]' 'name = "fixture"' 'version = "1.0.0"' \
    'edition = "2024"' 'license = "MIT"' \
    '[[bin]]' 'name = "fixture"' 'path = "main.rs"' \
    '[dependencies]' 'itoa = "=1.0.18"' \
    >"$TMP/signed-repo/Cargo.toml"
printf '%s\n' 'fn main() {}' >"$TMP/signed-repo/main.rs"
printf '%s\n' 'MIT License' >"$TMP/signed-repo/LICENSE"
printf '%s\n' '# notices' >"$TMP/signed-repo/NOTICE"
mkdir -p "$TMP/signed-repo/docs/legal" "$TMP/signed-repo/docs/releases"
printf '%s\n' '# notices' >"$TMP/signed-repo/docs/legal/third-party-notices.md"
printf '%s\n' '# provenance' >"$TMP/signed-repo/docs/legal/provenance.md"
printf '%s\n' '# additional terms' >"$TMP/signed-repo/docs/legal/additional-terms.md"
printf '%s\n' '# source availability' >"$TMP/signed-repo/docs/legal/source-availability.md"
printf '%s\n' '# release verification' >"$TMP/signed-repo/docs/releases/verification.md"
(cd "$TMP/signed-repo" && cargo generate-lockfile --offline >/dev/null)
mkdir "$TMP/signed-repo/scripts"
cp "$ROOT/scripts/go-proxy-evidence.py" "$TMP/signed-repo/scripts/"
cp "$ROOT/scripts/tesla-proxy-lock.json" "$TMP/signed-repo/scripts/"
cp "$ROOT/scripts/fleet-telemetry-evidence.py" "$TMP/signed-repo/scripts/"
cp "$ROOT/scripts/legal-bundle.py" "$TMP/signed-repo/scripts/"
cp "$ROOT/scripts/rust-source-evidence.py" "$TMP/signed-repo/scripts/"
cp "$ROOT/scripts/release-evidence.py" "$TMP/signed-repo/scripts/"
cat >"$TMP/signed-repo/scripts/debian-release-attestation.py" <<'PY'
#!/usr/bin/env python3
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("mode", choices=("verify",))
parser.add_argument("--repo")
parser.add_argument("--tag", required=True)
parser.add_argument("--tag-signer-fingerprint", required=True)
parser.add_argument("--package", type=Path, required=True)
parser.add_argument("--architecture", choices=("amd64", "arm64"), required=True)
parser.add_argument("--receipt", type=Path, required=True)
parser.add_argument("--signature", type=Path, required=True)
parser.add_argument("--public-key", type=Path, required=True)
parser.add_argument("--public-key-sha256", required=True)
args = parser.parse_args()
assert hashlib.sha256(args.public_key.read_bytes()).hexdigest() == args.public_key_sha256
receipt = json.loads(args.receipt.read_text())
assert receipt["schema"] == "teslatlas.debian-native-release-attestation/v1"
assert receipt["source"]["tag"] == args.tag
assert receipt["subject"] == {
    "architecture": args.architecture,
    "package_sha256": hashlib.sha256(args.package.read_bytes()).hexdigest(),
    "sidecars": {"fleet_telemetry": None, "go_proxy": None},
}
subprocess.run(
    [
        "openssl", "pkeyutl", "-verify", "-rawin", "-pubin",
        "-inkey", str(args.public_key), "-sigfile", str(args.signature),
        "-in", str(args.receipt),
    ],
    check=True,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)
PY
mkdir -p "$TMP/signed-repo/packaging/fleet-telemetry-bridge"
cp "$ROOT/packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json" \
    "$TMP/signed-repo/packaging/fleet-telemetry-bridge/"
cp "$ROOT/packaging/fleet-telemetry-bridge/fleet-telemetry-legal-lock.json" \
    "$TMP/signed-repo/packaging/fleet-telemetry-bridge/"
mkdir -p "$TMP/signed-repo/packaging/tesla-command-proxy"
cp "$ROOT/packaging/tesla-command-proxy/0001-go-1.27-runtime-defaults.patch" \
    "$TMP/signed-repo/packaging/tesla-command-proxy/"
mkdir "$TMP/signed-repo/LICENSES"
cp "$ROOT/LICENSES/Apache-2.0.txt" "$TMP/signed-repo/LICENSES/"
GNUPGHOME="$GPG_TMP" gpg --batch --armor --export "$TAG_FINGERPRINT" \
    >"$TMP/signed-repo/RELEASE_SIGNING_KEY.asc"
git -C "$TMP/signed-repo" add Cargo.toml main.rs LICENSE NOTICE docs \
    RELEASE_SIGNING_KEY.asc Cargo.lock scripts packaging LICENSES
git -C "$TMP/signed-repo" commit -q -m fixture
GNUPGHOME="$GPG_TMP" git -C "$TMP/signed-repo" tag -s -m fixture v1.0.0
write_fixture_deb() {
    python3 - "$1" "$TMP/signed-repo" "${2:-1.0.0-1}" \
        "${3:-amd64}" "${4:-62}" "${5:-0}" <<'PY'
from io import BytesIO
from pathlib import Path
import struct
import sys
import tarfile

output, repo = Path(sys.argv[1]), Path(sys.argv[2])
version, architecture = sys.argv[3], sys.argv[4]
machine, mutate_legal = int(sys.argv[5]), sys.argv[6] == "1"

def tar_bytes(files):
    target = BytesIO()
    with tarfile.open(fileobj=target, mode="w:gz") as archive:
        for name, (data, mode) in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = mode
            info.mtime = 0
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            archive.addfile(info, BytesIO(data))
    return target.getvalue()

control = (
    f"Package: teslatlas-hub\nVersion: {version}\n"
    f"Architecture: {architecture}\nDescription: fixture\n"
).encode()
control_tar = tar_bytes({"./control": (control, 0o644)})
elf = bytearray(64)
elf[:7] = b"\x7fELF\x02\x01\x01"
struct.pack_into("<H", elf, 16, 3)
struct.pack_into("<H", elf, 18, machine)
struct.pack_into("<I", elf, 20, 1)
legal = {
    "copyright": "LICENSE",
    "NOTICE": "NOTICE",
    "THIRD_PARTY_NOTICES.md": "docs/legal/third-party-notices.md",
    "PROVENANCE.md": "docs/legal/provenance.md",
    "ADDITIONAL_TERMS.md": "docs/legal/additional-terms.md",
    "SOURCE_AVAILABILITY.md": "docs/legal/source-availability.md",
    "RELEASE_VERIFICATION.md": "docs/releases/verification.md",
}
payload = {"./usr/bin/teslatlas-hub": (bytes(elf), 0o755)}
for packaged, source in legal.items():
    content = (repo / source).read_bytes()
    if mutate_legal and packaged == "NOTICE":
        content += b"changed\n"
    payload[f"./usr/share/doc/teslatlas-hub/{packaged}"] = (content, 0o644)
for component in (repo / "dependency-legal").iterdir():
    payload[f"./usr/share/doc/teslatlas-hub/dependency-legal/{component.name}"] = (
        component.read_bytes(), 0o644
    )
data_tar = tar_bytes(payload)

def ar_member(name, data):
    header = (
        f"{name}/".ljust(16)
        + "0".ljust(12)
        + "0".ljust(6)
        + "0".ljust(6)
        + "100644".ljust(8)
        + str(len(data)).ljust(10)
        + "`\n"
    ).encode("ascii")
    return header + data + (b"\n" if len(data) % 2 else b"")

output.write_bytes(
    b"!<arch>\n"
    + ar_member("debian-binary", b"2.0\n")
    + ar_member("control.tar.gz", control_tar)
    + ar_member("data.tar.gz", data_tar)
)
PY
}
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
    -out "$TMP/signing.pem" >/dev/null 2>&1
openssl pkey -in "$TMP/signing.pem" -pubout -out "$TMP/signing.pub" >/dev/null 2>&1
PUBLIC_KEY_SHA256=$(shasum -a 256 "$TMP/signing.pub" | awk '{ print $1 }')
python3 "$TMP/signed-repo/scripts/legal-bundle.py" --repo "$TMP/signed-repo" \
    --output-dir "$TMP/signed-repo/dependency-legal" >/dev/null
TEST_LEGAL_BUNDLE="$TMP/signed-repo/dependency-legal"
python3 "$TMP/signed-repo/scripts/rust-source-evidence.py" \
    --repo "$TMP/signed-repo" \
    --cargo-home "${CARGO_HOME:-$HOME/.cargo}" \
    --bin fixture \
    --output-dir "$TMP/rust-source-evidence" >/dev/null
TEST_RUST_SOURCE_EVIDENCE="$TMP/rust-source-evidence"
write_fixture_deb "$TMP/signed-repo/artifact.deb"
RELEASE_TEST_REAL_SCRIPT=$SCRIPT
export RELEASE_TEST_REAL_SCRIPT TEST_LEGAL_BUNDLE TEST_RUST_SOURCE_EVIDENCE
SCRIPT="$TMP/release-evidence-test-wrapper.py"
cat >"$SCRIPT" <<'PY'
import os
import runpy
import sys

script = os.environ["RELEASE_TEST_REAL_SCRIPT"]
sys.argv[0] = script
sys.argv.extend([
    "--legal-bundle", os.environ["TEST_LEGAL_BUNDLE"],
    "--rust-source-evidence", os.environ["TEST_RUST_SOURCE_EVIDENCE"],
])
runpy.run_path(script, run_name="__main__")
PY
openssl genpkey -algorithm ED25519 -out "$TMP/debian-attestation.pem" >/dev/null 2>&1
chmod 0600 "$TMP/debian-attestation.pem"
openssl pkey -in "$TMP/debian-attestation.pem" -pubout \
    -out "$TMP/debian-attestation-public.pem" >/dev/null 2>&1
DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256=$(
    shasum -a 256 "$TMP/debian-attestation-public.pem" | awk '{ print $1 }'
)
mkdir "$TMP/debian-attestation-amd64"
python3 - "$TMP/signed-repo/artifact.deb" \
    "$TMP/debian-attestation-amd64/debian-native-attestation.json" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

package, receipt = map(Path, sys.argv[1:])
value = {
    "schema": "teslatlas.debian-native-release-attestation/v1",
    "source": {"tag": "v1.0.0"},
    "subject": {
        "architecture": "amd64",
        "package_sha256": hashlib.sha256(package.read_bytes()).hexdigest(),
        "sidecars": {"fleet_telemetry": None, "go_proxy": None},
    },
}
receipt.write_text(json.dumps(value, separators=(",", ":"), sort_keys=True) + "\n")
PY
openssl pkeyutl -sign -rawin -inkey "$TMP/debian-attestation.pem" \
    -in "$TMP/debian-attestation-amd64/debian-native-attestation.json" \
    -out "$TMP/debian-attestation-amd64/debian-native-attestation.sig"

write_fixture_deb "$TMP/signed-repo/wrong-version.deb" 9.9.9-1
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/wrong-deb-version-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/wrong-version.deb" >"$TMP/wrong-deb-version.out" 2>&1; then
    echo 'release-evidence test: wrong Debian package version was accepted' >&2
    exit 1
fi
grep -Fq 'Debian package version does not match Cargo package version' \
    "$TMP/wrong-deb-version.out"
rm "$TMP/signed-repo/wrong-version.deb"

write_fixture_deb "$TMP/signed-repo/wrong-architecture.deb" 1.0.0-1 arm64 62
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/wrong-deb-architecture-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/wrong-architecture.deb" \
    >"$TMP/wrong-deb-architecture.out" 2>&1; then
    echo 'release-evidence test: wrong Debian package architecture was accepted' >&2
    exit 1
fi
grep -Fq 'Debian package architecture does not match its Hub binary' \
    "$TMP/wrong-deb-architecture.out"
rm "$TMP/signed-repo/wrong-architecture.deb"

write_fixture_deb "$TMP/signed-repo/wrong-legal.deb" 1.0.0-1 amd64 62 1
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/wrong-deb-legal-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/wrong-legal.deb" >"$TMP/wrong-deb-legal.out" 2>&1; then
    echo 'release-evidence test: wrong Debian legal payload was accepted' >&2
    exit 1
fi
grep -Fq 'Debian package legal payload mismatch: NOTICE' "$TMP/wrong-deb-legal.out"
rm "$TMP/signed-repo/wrong-legal.deb"

if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/deb-native-attestation-required" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.deb" >"$TMP/deb-native-attestation.out" 2>&1; then
    echo 'release-evidence test: Debian artifact without native attestation was accepted' >&2
    exit 1
fi
grep -Fq 'Debian packages require --debian-attestation-public-key' \
    "$TMP/deb-native-attestation.out"

ln -s "$TMP/debian-attestation-amd64" "$TMP/debian-attestation-link"
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/deb-symlink-attestation-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.deb" \
    --debian-attestation "$TMP/debian-attestation-link" \
    --debian-attestation-public-key "$TMP/debian-attestation-public.pem" \
    --debian-attestation-public-key-sha256 "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256" \
    >"$TMP/deb-symlink-attestation.out" 2>&1; then
    echo 'release-evidence test: symlinked Debian attestation was accepted' >&2
    exit 1
fi
grep -Fq 'real, non-symlink directory' "$TMP/deb-symlink-attestation.out"

cp -R "$TMP/debian-attestation-amd64" "$TMP/debian-attestation-mismatch"
python3 - "$TMP/debian-attestation-mismatch/debian-native-attestation.json" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
value = json.loads(path.read_text())
value["subject"]["package_sha256"] = "0" * 64
path.write_text(json.dumps(value, separators=(",", ":"), sort_keys=True) + "\n")
PY
openssl pkeyutl -sign -rawin -inkey "$TMP/debian-attestation.pem" \
    -in "$TMP/debian-attestation-mismatch/debian-native-attestation.json" \
    -out "$TMP/debian-attestation-mismatch/debian-native-attestation.sig"
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/deb-mismatch-attestation-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.deb" \
    --debian-attestation "$TMP/debian-attestation-mismatch" \
    --debian-attestation-public-key "$TMP/debian-attestation-public.pem" \
    --debian-attestation-public-key-sha256 "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256" \
    >"$TMP/deb-mismatch-attestation.out" 2>&1; then
    echo 'release-evidence test: mismatched Debian attestation was accepted' >&2
    exit 1
fi
grep -Fq 'package digest does not match its captured artifact' \
    "$TMP/deb-mismatch-attestation.out"

cp -R "$TMP/debian-attestation-amd64" "$TMP/debian-attestation-tampered"
python3 - "$TMP/debian-attestation-tampered/debian-native-attestation.sig" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
data = bytearray(path.read_bytes())
data[0] ^= 1
path.write_bytes(data)
PY
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/deb-tampered-attestation-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.deb" \
    --debian-attestation "$TMP/debian-attestation-tampered" \
    --debian-attestation-public-key "$TMP/debian-attestation-public.pem" \
    --debian-attestation-public-key-sha256 "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256" \
    >"$TMP/deb-tampered-attestation.out" 2>&1; then
    echo 'release-evidence test: tampered Debian attestation was accepted' >&2
    exit 1
fi
grep -Fq 'command failed' "$TMP/deb-tampered-attestation.out" \
    || { cat "$TMP/deb-tampered-attestation.out" >&2; exit 1; }

GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/deb-attested-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.deb" \
    --debian-attestation "$TMP/debian-attestation-amd64" \
    --debian-attestation-public-key "$TMP/debian-attestation-public.pem" \
    --debian-attestation-public-key-sha256 "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256" \
    >/dev/null
assert_flat_files "$TMP/deb-attested-evidence" \
    artifact.deb RELEASE_SIGNING_KEY.asc TeslatlasHubDebianAttestationPublicKey.pem \
    teslatlas-hub-v1.0.0-source.tar.gz teslatlas-hub-v1.0.0-evidence.tar.gz \
    SHA256SUMS SHA256SUMS.asc
(cd "$TMP/deb-attested-evidence" && shasum -a 256 -c SHA256SUMS >/dev/null)
mkdir "$TMP/deb-attested-detail"
tar -xzf "$TMP/deb-attested-evidence/teslatlas-hub-v1.0.0-evidence.tar.gz" \
    -C "$TMP/deb-attested-detail"
DEB_DETAIL="$TMP/deb-attested-detail/teslatlas-hub-v1.0.0-evidence"
[ -s "$DEB_DETAIL/debian-native-attestations/amd64/debian-native-attestation.json" ]
[ -s "$DEB_DETAIL/debian-native-attestations/amd64/debian-native-attestation.sig" ]
cmp "$TMP/debian-attestation-public.pem" \
    "$DEB_DETAIL/TeslatlasHubDebianAttestationPublicKey.pem"
cmp "$TMP/debian-attestation-public.pem" \
    "$TMP/deb-attested-evidence/TeslatlasHubDebianAttestationPublicKey.pem"
[ -s "$TMP/deb-attested-evidence/RELEASE_SIGNING_KEY.asc" ]
grep -Fq '  artifact.deb' "$TMP/deb-attested-evidence/SHA256SUMS"
if grep -Fq 'debian-native-attestations/' "$TMP/deb-attested-evidence/SHA256SUMS"; then
    echo 'release-evidence test: nested attestation path leaked into flat checksums' >&2
    exit 1
fi
rm "$TMP/signed-repo/artifact.deb"
printf '%s\n' artifact >"$TMP/signed-repo/artifact.bin"
ln "$TMP/signed-repo/artifact.bin" "$TMP/signed-repo/artifact-hardlink.bin"
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/hardlink-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.bin" >"$TMP/hardlink.out" 2>&1; then
    echo 'release-evidence test: hardlinked artifact was accepted' >&2
    exit 1
fi
grep -Fq 'regular, non-symlink file' "$TMP/hardlink.out"
rm "$TMP/signed-repo/artifact-hardlink.bin"

REAL_CARGO=$(command -v cargo)
mkdir "$TMP/mutate-bin"
cat >"$TMP/mutate-bin/cargo" <<EOF
#!/bin/sh
printf '%s\n' changed >>'$TMP/signed-repo/artifact.bin'
exec '$REAL_CARGO' "\$@"
EOF
chmod 700 "$TMP/mutate-bin/cargo"
if PATH="$TMP/mutate-bin:$PATH" GNUPGHOME="$GPG_TMP" \
    python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.bin" >"$TMP/mutation.out" 2>&1; then
    echo 'release-evidence test: artifact mutation was accepted' >&2
    exit 1
fi
grep -Eq 'artifact changed (during evidence generation|before staging|after reading)' \
    "$TMP/mutation.out" || { cat "$TMP/mutation.out" >&2; exit 1; }
[ ! -e "$TMP/signed-repo/evidence" ] \
    || { echo 'release-evidence test: failed generation exposed partial output' >&2; exit 1; }
printf '%s\n' artifact >"$TMP/signed-repo/artifact.bin"

GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.bin" >/dev/null
assert_flat_files "$TMP/signed-repo/evidence" \
    artifact.bin RELEASE_SIGNING_KEY.asc teslatlas-hub-v1.0.0-source.tar.gz \
    teslatlas-hub-v1.0.0-evidence.tar.gz SHA256SUMS SHA256SUMS.asc
[ -s "$TMP/signed-repo/evidence/teslatlas-hub-v1.0.0-source.tar.gz" ]
[ -s "$TMP/signed-repo/evidence/teslatlas-hub-v1.0.0-evidence.tar.gz" ]
[ -s "$TMP/signed-repo/evidence/RELEASE_SIGNING_KEY.asc" ]
[ -s "$TMP/signed-repo/evidence/SHA256SUMS.asc" ]
(cd "$TMP/signed-repo/evidence" && shasum -a 256 -c SHA256SUMS >/dev/null)
cp -R "$TMP/signed-repo/evidence" "$TMP/flat-download-copy"
(cd "$TMP/flat-download-copy" && shasum -a 256 -c SHA256SUMS >/dev/null)
mkdir "$TMP/evidence-detail"
tar -xzf "$TMP/signed-repo/evidence/teslatlas-hub-v1.0.0-evidence.tar.gz" \
    -C "$TMP/evidence-detail"
EVIDENCE_DETAIL="$TMP/evidence-detail/teslatlas-hub-v1.0.0-evidence"
[ -s "$EVIDENCE_DETAIL/sbom.spdx.json" ]
[ -s "$EVIDENCE_DETAIL/THIRD_PARTY_NOTICES.generated.md" ]
[ -s "$EVIDENCE_DETAIL/rust-source-evidence/rust-vendored-sources.tar.gz" ]
GNUPGHOME="$GPG_TMP" gpg --batch --status-fd=1 \
    --verify "$TMP/signed-repo/evidence/SHA256SUMS.asc" \
    "$TMP/signed-repo/evidence/SHA256SUMS" 2>/dev/null \
    | grep -Fq "[GNUPG:] VALIDSIG $TAG_FINGERPRINT "
openssl dgst -sha256 -verify "$EVIDENCE_DETAIL/provenance-public-key.pem" \
    -signature "$EVIDENCE_DETAIL/provenance.sig" \
    "$EVIDENCE_DETAIL/provenance.json" >/dev/null
MANIFEST_ARTIFACT_SHA=$(python3 -c \
    'import json,sys; print(json.load(open(sys.argv[1]))["artifacts"][0]["sha256"])' \
    "$EVIDENCE_DETAIL/artifact-manifest.json")
CHECKSUM_ARTIFACT_SHA=$(awk '$2 == "artifact.bin" { print $1 }' \
    "$TMP/signed-repo/evidence/SHA256SUMS")
[ "$MANIFEST_ARTIFACT_SHA" = "$CHECKSUM_ARTIFACT_SHA" ] \
    || { echo 'release-evidence test: manifest/checksum artifact digests differ' >&2; exit 1; }
mv "$TMP/signed-repo/evidence" "$TMP/evidence-finished"

rm "$TMP/signed-repo/artifact.bin"
printf '%s\n' mac-artifact >"$TMP/signed-repo/artifact.zip"
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/missing-go-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" >"$TMP/missing-go.out" 2>&1; then
    echo 'release-evidence test: macOS artifact without Go evidence was accepted' >&2
    exit 1
fi
grep -Fq 'macOS artifacts require --go-proxy-evidence' "$TMP/missing-go.out"

if [ -n "${TESLATLAS_TEST_GO_PROXY:-}" ] || [ -n "${TESLATLAS_TEST_GO_EVIDENCE:-}" ]; then
    [ -n "${TESLATLAS_TEST_GO_PROXY:-}" ] \
        && [ -n "${TESLATLAS_TEST_GO_EVIDENCE:-}" ] \
        || { echo 'release-evidence test: both cached Go inputs are required' >&2; exit 1; }
    cp "$TESLATLAS_TEST_GO_PROXY" "$TMP/tesla-http-proxy"
    cp -R "$TESLATLAS_TEST_GO_EVIDENCE" "$TMP/go-evidence"
else
    "$ROOT/scripts/build-tesla-command-proxy.sh" \
        --output "$TMP/tesla-http-proxy" >/dev/null
    python3 "$ROOT/scripts/go-proxy-evidence.py" --repo "$ROOT" \
        --proxy-binary "$TMP/tesla-http-proxy" \
        --output-dir "$TMP/go-evidence" >/dev/null
fi
python3 "$TMP/signed-repo/scripts/go-proxy-evidence.py" \
    --repo "$TMP/signed-repo" --verify-dir "$TMP/go-evidence" >/dev/null
cp "$TMP/tesla-http-proxy" "$TMP/fleet-telemetry"
python3 "$ROOT/scripts/fleet-telemetry-evidence.py" --repo "$ROOT" \
    --receiver-binary "$TMP/fleet-telemetry" --output-dir "$TMP/fleet-telemetry-evidence" >/dev/null
python3 "$ROOT/scripts/fleet-telemetry-evidence.py" --repo "$ROOT" \
    --verify-dir "$TMP/fleet-telemetry-evidence" >/dev/null
python3 "$TMP/signed-repo/scripts/legal-bundle.py" --repo "$TMP/signed-repo" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" \
    --output-dir "$TMP/sidecar-dependency-legal" >/dev/null
TEST_LEGAL_BUNDLE="$TMP/sidecar-dependency-legal"
find "$TMP/signed-repo/dependency-legal" -depth -delete
mkdir -p "$TMP/substituted-repo/packaging/fleet-telemetry-bridge"
cp "$ROOT/packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json" \
    "$TMP/substituted-repo/packaging/fleet-telemetry-bridge/"
cp "$ROOT/packaging/fleet-telemetry-bridge/fleet-telemetry-legal-lock.json" \
    "$TMP/substituted-repo/packaging/fleet-telemetry-bridge/"
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
    --output-dir "$TMP/substituted-fleet-telemetry-evidence" \
    >"$TMP/substituted-fleet-telemetry.out" 2>&1; then
    echo 'release-evidence test: substituted Fleet Telemetry lock was accepted' >&2
    exit 1
fi
grep -Fq 'legal lock does not bind the bridge lock' \
    "$TMP/substituted-fleet-telemetry.out"

if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/forged-mac-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" >"$TMP/forged-mac.out" 2>&1; then
    echo 'release-evidence test: arbitrary ZIP was accepted as a macOS release' >&2
    exit 1
fi
grep -Eq 'valid ZIP archive|exactly one app' "$TMP/forged-mac.out" \
    || { cat "$TMP/forged-mac.out" >&2; exit 1; }
[ ! -e "$TMP/signed-repo/forged-mac-evidence" ]

rm "$TMP/signed-repo/artifact.zip"
mkdir -p "$TMP/mac-artifact/Teslatlas Hub.app/Contents/MacOS" \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources"
printf '%s\n' '#!/bin/sh' 'exit 0' \
    >"$TMP/mac-artifact/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub"
chmod 0755 "$TMP/mac-artifact/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub"
cp "$TMP/tesla-http-proxy" \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/tesla-http-proxy"
cp "$TMP/fleet-telemetry" \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/fleet-telemetry"
printf '%s\n' '#!/bin/sh' \
    'printf "%s\n" "teslatlas-hub ${RELEASE_TEST_HUB_VERSION:-1.0.0}"' \
    >"$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/teslatlas-hub"
chmod 0755 "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/teslatlas-hub"
printf '%s\n' service-package \
    >"$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg"
for legal_entry in \
    'LICENSE|LICENSE' \
    'NOTICE|NOTICE' \
    'THIRD_PARTY_NOTICES.md|docs/legal/third-party-notices.md' \
    'PROVENANCE.md|docs/legal/provenance.md' \
    'ADDITIONAL_TERMS.md|docs/legal/additional-terms.md' \
    'SOURCE_AVAILABILITY.md|docs/legal/source-availability.md' \
    'RELEASE_VERIFICATION.md|docs/releases/verification.md'; do
    legal_name=${legal_entry%%|*}
    legal_source=${legal_entry#*|}
    cp "$TMP/signed-repo/$legal_source" \
        "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/$legal_name"
done
/usr/bin/ditto --noextattr --norsrc "$TEST_LEGAL_BUNDLE" \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/DependencyLegal"
python3 - "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Info.plist" \
    "$TMP/tesla-http-proxy" "$TMP/fleet-telemetry" "$TMP/go-evidence/go-component-manifest.json" \
    "$TMP/fleet-telemetry-evidence/fleet-telemetry-component-manifest.json" \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg" \
    "$TEST_LEGAL_BUNDLE/legal-bundle-manifest.json" <<'PY'
import hashlib
from pathlib import Path
import plistlib
import sys

info, proxy, receiver, evidence, receiver_evidence, package, legal_manifest = map(Path, sys.argv[1:])
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
info.write_bytes(plistlib.dumps({
    "CFBundleExecutable": "Teslatlas Hub",
    "CFBundleIdentifier": "eu.teslatlas.hub.fixture",
    "CFBundlePackageType": "APPL",
    "CFBundleShortVersionString": "1.0.0",
    "CFBundleVersion": "1.0.0",
    "TeslatlasHubVersion": "1.0.0",
    "TeslatlasOfficialRelease": True,
    "TeslatlasReleaseTeamIdentifier": "4AA2EMZ2HA",
    "TeslatlasUnsignedProxySHA256": sha(proxy),
    "TeslatlasGoEvidenceManifestSHA256": sha(evidence),
    "TeslatlasUnsignedFleetTelemetrySHA256": sha(receiver),
    "TeslatlasFleetTelemetryEvidenceManifestSHA256": sha(receiver_evidence),
    "TeslatlasServicePackageSHA256": sha(package),
    "TeslatlasLegalBundleManifestSHA256": sha(legal_manifest),
}))
PY
(cd "$TMP/mac-artifact" && /usr/bin/ditto -c -k --noextattr --norsrc --keepParent \
    'Teslatlas Hub.app' "$TMP/signed-repo/artifact.zip")
cp "$TMP/signed-repo/artifact.zip" "$TMP/good-artifact.zip"
python3 - "$TMP/signed-repo/artifact.zip" <<'PY'
from pathlib import Path
import sys
import zipfile

with zipfile.ZipFile(Path(sys.argv[1]), "a") as archive:
    archive.writestr("Install.command", b"unsigned payload\n")
PY
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/extra-zip-content" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" >"$TMP/extra-zip-content.out" 2>&1; then
    echo 'release-evidence test: unsigned ZIP sidecar was accepted' >&2
    exit 1
fi
grep -Fq 'content outside the signed app' "$TMP/extra-zip-content.out"
[ ! -e "$TMP/signed-repo/extra-zip-content" ]
cp "$TMP/good-artifact.zip" "$TMP/signed-repo/artifact.zip"
mkdir "$TMP/mac-bin"
cat >"$TMP/mac-bin/codesign" <<'EOF'
#!/bin/sh
case "$1" in
    --verify) exit 0 ;;
    --remove-signature) exit 0 ;;
    -d)
        printf '%s\n' \
            'Authority=Developer ID Application: Fixture (4AA2EMZ2HA)' \
            'TeamIdentifier=4AA2EMZ2HA' >&2
        exit 0
        ;;
esac
exit 2
EOF
cat >"$TMP/mac-bin/pkgutil" <<'EOF'
#!/bin/sh
case "$1" in
    --check-signature)
        printf '%s\n' \
            'Status: signed by a certificate trusted by macOS' \
            'Developer ID Installer: Fixture (4AA2EMZ2HA)'
        ;;
    --expand-full)
        mkdir -p "$3/Payload/Library/Application Support/Teslatlas Hub/bin" \
            "$3/Payload/Library/Application Support/Teslatlas Hub/share"
        printf '%s\n' '#!/bin/sh' \
            'printf "%s\n" "teslatlas-hub ${RELEASE_TEST_HUB_VERSION:-1.0.0}"' \
            > "$3/Payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
        chmod 0755 "$3/Payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
        cp "$RELEASE_TEST_PROXY" \
            "$3/Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
        cp "$RELEASE_TEST_FLEET_TELEMETRY" \
            "$3/Payload/Library/Application Support/Teslatlas Hub/bin/fleet-telemetry"
        for legal_entry in \
            'LICENSE|LICENSE' \
            'NOTICE|NOTICE' \
            'THIRD_PARTY_NOTICES.md|docs/legal/third-party-notices.md' \
            'PROVENANCE.md|docs/legal/provenance.md' \
            'ADDITIONAL_TERMS.md|docs/legal/additional-terms.md' \
            'SOURCE_AVAILABILITY.md|docs/legal/source-availability.md' \
            'RELEASE_VERIFICATION.md|docs/releases/verification.md'; do
            legal_name=${legal_entry%%|*}
            legal_source=${legal_entry#*|}
            cp "$RELEASE_TEST_LEGAL_REPO/$legal_source" \
                "$3/Payload/Library/Application Support/Teslatlas Hub/share/$legal_name"
        done
        cp -R "$RELEASE_TEST_LEGAL_BUNDLE" \
            "$3/Payload/Library/Application Support/Teslatlas Hub/share/dependency-legal"
        if [ "${RELEASE_MUTATE_PACKAGE_LEGAL:-0}" = 1 ]; then
            printf '%s\n' changed >> \
                "$3/Payload/Library/Application Support/Teslatlas Hub/share/NOTICE"
        fi
        if [ "${RELEASE_MUTATE_PACKAGE_PROXY:-0}" = 1 ]; then
            printf '%s\n' changed >> \
                "$3/Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
        fi
        printf '%s\n' \
            "<pkg-info identifier=\"${RELEASE_TEST_PACKAGE_IDENTIFIER:-com.teslatlas.hub.service}\" version=\"${RELEASE_TEST_PACKAGE_VERSION:-1.0.0}\" install-location=\"${RELEASE_TEST_PACKAGE_LOCATION:-/}\"/>" \
            > "$3/PackageInfo"
        ;;
    *) exit 2 ;;
esac
EOF
cat >"$TMP/mac-bin/spctl" <<'EOF'
#!/bin/sh
[ "${RELEASE_REJECT_GATEKEEPER:-0}" != 1 ]
EOF
cat >"$TMP/mac-bin/xcrun" <<'EOF'
#!/bin/sh
[ "$1" = stapler ] && [ "$2" = validate ] || exit 2
[ "${RELEASE_REJECT_STAPLER:-0}" != 1 ]
EOF
chmod 0755 "$TMP/mac-bin/codesign" "$TMP/mac-bin/pkgutil" \
    "$TMP/mac-bin/spctl" "$TMP/mac-bin/xcrun"
PATH="$TMP/mac-bin:$PATH"
export PATH
RELEASE_TEST_PROXY="$TMP/tesla-http-proxy"
export RELEASE_TEST_PROXY
RELEASE_TEST_FLEET_TELEMETRY="$TMP/fleet-telemetry"
export RELEASE_TEST_FLEET_TELEMETRY
RELEASE_TEST_LEGAL_REPO="$TMP/signed-repo"
export RELEASE_TEST_LEGAL_REPO
RELEASE_TEST_LEGAL_BUNDLE="$TEST_LEGAL_BUNDLE"
export RELEASE_TEST_LEGAL_BUNDLE

printf '%s\n' changed >> \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/NOTICE"
rm "$TMP/signed-repo/artifact.zip"
(cd "$TMP/mac-artifact" && /usr/bin/ditto -c -k --noextattr --norsrc --keepParent \
    'Teslatlas Hub.app' "$TMP/signed-repo/artifact.zip")
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-mac-app-legal" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" \
    >"$TMP/wrong-mac-app-legal.out" 2>&1; then
    echo 'release-evidence test: wrong macOS app legal payload was accepted' >&2
    exit 1
fi
grep -Fq 'macOS app legal payload mismatch: NOTICE' "$TMP/wrong-mac-app-legal.out" \
    || { cat "$TMP/wrong-mac-app-legal.out" >&2; exit 1; }
[ ! -e "$TMP/signed-repo/wrong-mac-app-legal" ]
cp "$TMP/good-artifact.zip" "$TMP/signed-repo/artifact.zip"
cp "$TMP/signed-repo/NOTICE" \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/NOTICE"

if RELEASE_TEST_HUB_VERSION=9.9.9 GNUPGHOME="$GPG_TMP" \
    python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-mac-hub-version" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" \
    >"$TMP/wrong-mac-hub-version.out" 2>&1; then
    echo 'release-evidence test: wrong macOS Hub binary version was accepted' >&2
    exit 1
fi
grep -Fq 'macOS app Hub binary version does not match Cargo package version' \
    "$TMP/wrong-mac-hub-version.out"
[ ! -e "$TMP/signed-repo/wrong-mac-hub-version" ]

if RELEASE_TEST_PACKAGE_VERSION=9.9.9 GNUPGHOME="$GPG_TMP" \
    python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-mac-package-version" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" \
    >"$TMP/wrong-mac-package-version.out" 2>&1; then
    echo 'release-evidence test: wrong macOS package version was accepted' >&2
    exit 1
fi
grep -Fq 'macOS service package version does not match Cargo package version' \
    "$TMP/wrong-mac-package-version.out"
[ ! -e "$TMP/signed-repo/wrong-mac-package-version" ]

for package_metadata_case in identifier location; do
    case "$package_metadata_case" in
        identifier)
            package_metadata_env='RELEASE_TEST_PACKAGE_IDENTIFIER=invalid.example'
            package_metadata_message='macOS service package identifier is invalid'
            ;;
        location)
            package_metadata_env='RELEASE_TEST_PACKAGE_LOCATION=/tmp'
            package_metadata_message='macOS service package install location is invalid'
            ;;
    esac
    if env "$package_metadata_env" GNUPGHOME="$GPG_TMP" \
        python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
        --tag-signer-fingerprint "$TAG_FINGERPRINT" \
        --output-dir "$TMP/signed-repo/wrong-mac-package-$package_metadata_case" \
        --signing-key "$TMP/signing.pem" --public-key-sha256 "$PUBLIC_KEY_SHA256" \
        --artifact "$TMP/signed-repo/artifact.zip" \
        --go-proxy-evidence "$TMP/go-evidence" \
        --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" \
        >"$TMP/wrong-mac-package-$package_metadata_case.out" 2>&1; then
        echo "release-evidence test: wrong macOS package $package_metadata_case was accepted" >&2
        exit 1
    fi
    grep -Fq "$package_metadata_message" \
        "$TMP/wrong-mac-package-$package_metadata_case.out"
done

if RELEASE_MUTATE_PACKAGE_LEGAL=1 GNUPGHOME="$GPG_TMP" \
    python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-mac-package-legal" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" \
    >"$TMP/wrong-mac-package-legal.out" 2>&1; then
    echo 'release-evidence test: wrong macOS package legal payload was accepted' >&2
    exit 1
fi
grep -Fq 'macOS package legal payload mismatch: NOTICE' \
    "$TMP/wrong-mac-package-legal.out"
[ ! -e "$TMP/signed-repo/wrong-mac-package-legal" ]

if RELEASE_REJECT_GATEKEEPER=1 GNUPGHOME="$GPG_TMP" \
    python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/gatekeeper-rejected" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" >"$TMP/gatekeeper-rejected.out" 2>&1; then
    echo 'release-evidence test: Gatekeeper rejection was accepted' >&2
    exit 1
fi
grep -Fq 'command failed: spctl' "$TMP/gatekeeper-rejected.out"
[ ! -e "$TMP/signed-repo/gatekeeper-rejected" ]

if RELEASE_REJECT_STAPLER=1 GNUPGHOME="$GPG_TMP" \
    python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/stapler-rejected" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" >"$TMP/stapler-rejected.out" 2>&1; then
    echo 'release-evidence test: notarization rejection was accepted' >&2
    exit 1
fi
grep -Fq 'command failed: xcrun' "$TMP/stapler-rejected.out"
[ ! -e "$TMP/signed-repo/stapler-rejected" ]

printf '%s\n' changed >> \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/tesla-http-proxy"
rm "$TMP/signed-repo/artifact.zip"
(cd "$TMP/mac-artifact" && /usr/bin/ditto -c -k --noextattr --norsrc --keepParent \
    'Teslatlas Hub.app' "$TMP/signed-repo/artifact.zip")
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-app-proxy" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" >"$TMP/wrong-app-proxy.out" 2>&1; then
    echo 'release-evidence test: mismatched app proxy was accepted' >&2
    exit 1
fi
grep -Fq 'app Tesla proxy does not match the reviewed unsigned proxy' \
    "$TMP/wrong-app-proxy.out"
[ ! -e "$TMP/signed-repo/wrong-app-proxy" ]
cp "$TMP/good-artifact.zip" "$TMP/signed-repo/artifact.zip"
cp "$TMP/tesla-http-proxy" \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/tesla-http-proxy"

if RELEASE_MUTATE_PACKAGE_PROXY=1 GNUPGHOME="$GPG_TMP" \
    python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-package-proxy" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" >"$TMP/wrong-package-proxy.out" 2>&1; then
    echo 'release-evidence test: mismatched package proxy was accepted' >&2
    exit 1
fi
grep -Fq 'package Tesla proxy does not match the reviewed unsigned proxy' \
    "$TMP/wrong-package-proxy.out"
[ ! -e "$TMP/signed-repo/wrong-package-proxy" ]

cp "$TMP/go-evidence/go-build-receipt.json" "$TMP/go-build-receipt.original"
cat >"$TMP/mutate-bin/cargo" <<'EOF'
#!/bin/sh
printf '%s\n' changed >>"$RELEASE_MUTATION_GO_EVIDENCE"
exec "$RELEASE_REAL_CARGO" "$@"
EOF
chmod 700 "$TMP/mutate-bin/cargo"
if PATH="$TMP/mutate-bin:$PATH" RELEASE_REAL_CARGO="$REAL_CARGO" \
    RELEASE_MUTATION_GO_EVIDENCE="$TMP/go-evidence/go-build-receipt.json" \
    GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/mutated-go-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" >"$TMP/go-mutation.out" 2>&1; then
    echo 'release-evidence test: Go evidence mutation was accepted' >&2
    exit 1
fi
grep -Eq 'Go proxy evidence changed (during evidence generation|before staging)|Go evidence component does not match its manifest|artifact changed before staging' \
    "$TMP/go-mutation.out"
[ ! -e "$TMP/signed-repo/mutated-go-evidence" ]
cp "$TMP/go-build-receipt.original" "$TMP/go-evidence/go-build-receipt.json"

GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/mac-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.zip" \
    --go-proxy-evidence "$TMP/go-evidence" \
    --fleet-telemetry-evidence "$TMP/fleet-telemetry-evidence" >/dev/null
assert_flat_files "$TMP/signed-repo/mac-evidence" \
    artifact.zip RELEASE_SIGNING_KEY.asc teslatlas-hub-v1.0.0-source.tar.gz \
    teslatlas-hub-v1.0.0-evidence.tar.gz SHA256SUMS SHA256SUMS.asc
(cd "$TMP/signed-repo/mac-evidence" && shasum -a 256 -c SHA256SUMS >/dev/null)
mkdir "$TMP/mac-evidence-detail"
tar -xzf "$TMP/signed-repo/mac-evidence/teslatlas-hub-v1.0.0-evidence.tar.gz" \
    -C "$TMP/mac-evidence-detail"
MAC_DETAIL="$TMP/mac-evidence-detail/teslatlas-hub-v1.0.0-evidence"
[ -s "$MAC_DETAIL/go-proxy-evidence/go-component-manifest.json" ]
python3 - "$MAC_DETAIL" <<'PY'
import json
from pathlib import Path
import sys

evidence = Path(sys.argv[1])
manifest = json.loads((evidence / "artifact-manifest.json").read_text())
provenance = json.loads((evidence / "provenance.json").read_text())
assert manifest["go_proxy_evidence"]["subject"]["name"] == "tesla-http-proxy"
assert provenance["go_proxy_evidence"] == manifest["go_proxy_evidence"]
PY
mv "$TMP/signed-repo/mac-evidence" "$TMP/mac-evidence-finished"
rm "$TMP/signed-repo/artifact.zip"

bundle="$TMP/signed-repo/TeslatlasHub-macOS"
mkdir -p "$bundle/notary-logs"
cp "$TMP/good-artifact.zip" "$bundle/Teslatlas Hub.zip"
cp "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg" \
    "$bundle/TeslatlasHubService.pkg"
cp -R "$TMP/go-evidence" "$bundle/go-proxy-evidence"
cp -R "$TMP/fleet-telemetry-evidence" "$bundle/fleet-telemetry-evidence"
cp -R "$TEST_LEGAL_BUNDLE" "$bundle/dependency-legal"
TEST_LEGAL_BUNDLE="$bundle/dependency-legal"
RELEASE_TEST_LEGAL_BUNDLE="$TEST_LEGAL_BUNDLE"
export RELEASE_TEST_LEGAL_BUNDLE
python3 - "$bundle" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

bundle = Path(sys.argv[1])
receipts = bundle / "notary-logs"
for label, artifact, archive_name, job_id in (
    (
        "app",
        bundle / "Teslatlas Hub.zip",
        "Teslatlas Hub-submission.zip",
        "11111111-1111-1111-1111-111111111111",
    ),
    (
        "service-package",
        bundle / "TeslatlasHubService.pkg",
        "TeslatlasHubService.pkg",
        "22222222-2222-2222-2222-222222222222",
    ),
):
    digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
    (receipts / f"{label}-submit.json").write_text(json.dumps({
        "id": job_id,
        "message": "Processing complete",
        "status": "Accepted",
    }) + "\n")
    (receipts / f"{label}-log.json").write_text(json.dumps({
        "logFormatVersion": 1,
        "jobId": job_id,
        "status": "Accepted",
        "statusSummary": "Ready for distribution",
        "statusCode": 0,
        "archiveFilename": archive_name,
        "sha256": digest,
        "ticketContents": [{
            "path": f"{archive_name}/fixture",
            "digestAlgorithm": "SHA-256",
            "cdhash": "a" * 40,
            "arch": "arm64",
        }],
        "issues": None,
    }) + "\n")
PY
(
    cd "$bundle"
    {
        shasum -a 256 'Teslatlas Hub.zip' TeslatlasHubService.pkg
        find go-proxy-evidence -type f -print | LC_ALL=C sort | while IFS= read -r file; do
            shasum -a 256 "$file"
        done
        find fleet-telemetry-evidence -type f -print | LC_ALL=C sort | while IFS= read -r file; do
            shasum -a 256 "$file"
        done
        find dependency-legal -type f -print | LC_ALL=C sort | while IFS= read -r file; do
            shasum -a 256 "$file"
        done
    } >SHA256SUMS
)
cp "$bundle/notary-logs/app-log.json" "$TMP/app-log.good.json"
printf '%s\n' '{"status":"Accepted"}' >"$bundle/notary-logs/app-log.json"
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/weak-notary-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$bundle/Teslatlas Hub.zip" \
    --artifact "$bundle/TeslatlasHubService.pkg" \
    --go-proxy-evidence "$bundle/go-proxy-evidence" \
    --fleet-telemetry-evidence "$bundle/fleet-telemetry-evidence" \
    >"$TMP/weak-notary.out" 2>&1; then
    echo 'release-evidence test: arbitrary Accepted notary JSON was accepted' >&2
    exit 1
fi
grep -Fq 'notary detail receipt is invalid' "$TMP/weak-notary.out"
cp "$TMP/app-log.good.json" "$bundle/notary-logs/app-log.json"

GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/bundle-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$bundle/Teslatlas Hub.zip" \
    --artifact "$bundle/TeslatlasHubService.pkg" \
    --go-proxy-evidence "$bundle/go-proxy-evidence" \
    --fleet-telemetry-evidence "$bundle/fleet-telemetry-evidence" >/dev/null
assert_flat_files "$TMP/signed-repo/bundle-evidence" \
    'Teslatlas Hub.zip' TeslatlasHubService.pkg RELEASE_SIGNING_KEY.asc \
    teslatlas-hub-v1.0.0-source.tar.gz teslatlas-hub-v1.0.0-evidence.tar.gz \
    SHA256SUMS SHA256SUMS.asc
(cd "$TMP/signed-repo/bundle-evidence" && shasum -a 256 -c SHA256SUMS >/dev/null)
mkdir "$TMP/bundle-evidence-detail"
tar -xzf "$TMP/signed-repo/bundle-evidence/teslatlas-hub-v1.0.0-evidence.tar.gz" \
    -C "$TMP/bundle-evidence-detail"
BUNDLE_DETAIL="$TMP/bundle-evidence-detail/teslatlas-hub-v1.0.0-evidence"
[ -s "$BUNDLE_DETAIL/macos-release-receipts/SHA256SUMS" ]
[ -s "$BUNDLE_DETAIL/macos-release-receipts/notary-logs/app-log.json" ]
mv "$TMP/signed-repo/bundle-evidence" "$TMP/bundle-evidence-finished"
printf '%s\n' unexpected >"$bundle/Install.command"
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/mutated-bundle-evidence" \
    --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$bundle/Teslatlas Hub.zip" \
    --artifact "$bundle/TeslatlasHubService.pkg" \
    --go-proxy-evidence "$bundle/go-proxy-evidence" \
    --fleet-telemetry-evidence "$bundle/fleet-telemetry-evidence" \
    >"$TMP/mutated-bundle.out" 2>&1; then
    echo 'release-evidence test: mutated release bundle was accepted' >&2
    exit 1
fi
grep -Fq 'macOS release bundle contains unexpected or missing sidecars' \
    "$TMP/mutated-bundle.out"
rm "$bundle/Install.command"
mv "$bundle" "$TMP/release-bundle-finished"
python3 "$TMP/signed-repo/scripts/legal-bundle.py" \
    --repo "$TMP/signed-repo" --output-dir "$TMP/base-legal-late" >/dev/null
TEST_LEGAL_BUNDLE="$TMP/base-legal-late"
RELEASE_TEST_LEGAL_BUNDLE="$TEST_LEGAL_BUNDLE"
export TEST_LEGAL_BUNDLE RELEASE_TEST_LEGAL_BUNDLE

printf '%s\n' 'fn main() { println!("later"); }' >"$TMP/signed-repo/main.rs"
git -C "$TMP/signed-repo" add main.rs
git -C "$TMP/signed-repo" commit -q -m later
printf '%s\n' later-artifact >"$TMP/signed-repo/later-artifact.bin"
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-head" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/later-artifact.bin" >"$TMP/wrong-head.out" 2>&1; then
    echo 'release-evidence test: clean HEAD outside signed tag was accepted' >&2
    exit 1
fi
grep -Fq 'candidate HEAD does not match the signed tag commit' "$TMP/wrong-head.out" \
    || { cat "$TMP/wrong-head.out" >&2; exit 1; }
[ ! -e "$TMP/signed-repo/wrong-head" ]

python3 - "$RELEASE_TEST_REAL_SCRIPT" "$TMP" <<'PY'
import importlib.util
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location("release_evidence_test", sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
root = Path(sys.argv[2])

toml_repo = root / "strict-toml-repo"
toml_repo.mkdir()
manifest = toml_repo / "Cargo.toml"
manifest.write_text(
    "[package]\n"
    'name = "fixture"\n'
    'version = "1.2.3-beta.4" # release version\n'
    "authors = [\"Fixture\"]\n\n"
    "[dependencies]\n"
    'serde = "1"\n'
)
assert module.cargo_package_version(toml_repo) == "1.2.3-beta.4"
for invalid_manifest in (
    '[package]\nversion = "1.2.3"\nversion = "1.2.4"\n',
    '[package]\nversion = "1.2.3"\n\n[package]\nname = "duplicate"\n',
    '[package]\nversion = { workspace = true }\n',
    '[package]\ndescription = """multiline\ntext"""\nversion = "1.2.3"\n',
    '[package]\nversion = "1.2.3"',
):
    manifest.write_text(invalid_manifest)
    try:
        module.cargo_package_version(toml_repo)
    except module.GateError:
        pass
    else:
        raise AssertionError(f"unsafe Cargo manifest accepted: {invalid_manifest!r}")

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

stage = root / "atomic-stage"
output = root / "atomic-output"
stage.mkdir()
(stage / "first").write_text("one")
(stage / "second").write_text("two")
module.publish_evidence_directory(stage, output)
assert not stage.exists()
assert sorted(path.name for path in output.iterdir()) == ["first", "second"]

collision_stage = root / "collision-stage"
collision_output = root / "collision-output"
collision_stage.mkdir()
collision_output.mkdir()
(collision_stage / "new").write_text("new")
(collision_output / "existing").write_text("existing")
try:
    module.publish_evidence_directory(collision_stage, collision_output)
except module.GateError:
    pass
else:
    raise AssertionError("existing output was replaced")
assert (collision_stage / "new").read_text() == "new"
assert (collision_output / "existing").read_text() == "existing"

source = root / "witness-source"
source.write_bytes(b"descriptor-pinned evidence\n")
witness = module.capture_artifact(root, source)
copied = root / "witness-copy"
module.copy_witness_to(root, witness, copied)
assert copied.read_bytes() == source.read_bytes()

parsed = module.architecture_evidence_directories(
    ["amd64=/tmp/go-amd64", "arm64=/tmp/go-arm64"],
    "--linux-go-proxy-evidence",
)
assert set(parsed) == {"amd64", "arm64"}
assert parsed["amd64"].is_absolute()
for invalid in (
    ["amd64"],
    ["x86_64=/tmp/evidence"],
    ["amd64="],
    ["amd64=/tmp/one", "amd64=/tmp/two"],
):
    try:
        module.architecture_evidence_directories(
            invalid, "--linux-go-proxy-evidence"
        )
    except module.GateError:
        pass
    else:
        raise AssertionError(f"invalid architecture evidence mapping accepted: {invalid}")

module.require_linux_evidence_coverage(
    {"amd64"}, {"amd64": root}, {"amd64": root}
)
for go_directories, fleet_directories in (
    ({}, {"amd64": root}),
    ({"amd64": root}, {}),
    ({"amd64": root, "arm64": root}, {"amd64": root}),
):
    try:
        module.require_linux_evidence_coverage(
            {"amd64"}, go_directories, fleet_directories
        )
    except module.GateError:
        pass
    else:
        raise AssertionError("incomplete or extra Linux sidecar evidence was accepted")

packaged_sidecars = {
    "go_proxy": {
        "name": "tesla-http-proxy",
        "sha256": "1" * 64,
        "size": 101,
    },
    "fleet_telemetry": {
        "name": "fleet-telemetry",
        "sha256": "2" * 64,
        "size": 202,
    },
}
go_manifest = {
    "target": "linux-amd64",
    "subject": dict(packaged_sidecars["go_proxy"]),
}
fleet_manifest = {
    "subject": {
        "target": "linux-amd64",
        **packaged_sidecars["fleet_telemetry"],
    }
}
module.validate_linux_sidecar_evidence_subjects(
    "amd64", go_manifest, fleet_manifest, packaged_sidecars
)
invalid_manifest_pairs = []
wrong_go_target = {**go_manifest, "target": "linux-arm64"}
invalid_manifest_pairs.append((wrong_go_target, fleet_manifest))
wrong_fleet_target = {
    "subject": {**fleet_manifest["subject"], "target": "linux-arm64"}
}
invalid_manifest_pairs.append((go_manifest, wrong_fleet_target))
wrong_go_digest = {
    **go_manifest,
    "subject": {**go_manifest["subject"], "sha256": "3" * 64},
}
invalid_manifest_pairs.append((wrong_go_digest, fleet_manifest))
cross_architecture = {
    "target": "linux-arm64",
    "subject": dict(packaged_sidecars["go_proxy"]),
}
invalid_manifest_pairs.append((cross_architecture, fleet_manifest))
for invalid_go, invalid_fleet in invalid_manifest_pairs:
    try:
        module.validate_linux_sidecar_evidence_subjects(
            "amd64", invalid_go, invalid_fleet, packaged_sidecars
        )
    except module.GateError:
        pass
    else:
        raise AssertionError("mismatched Linux sidecar evidence was accepted")

help_records = {
    "go_proxy": {
        **packaged_sidecars["go_proxy"],
        "path": "usr/lib/teslatlas-hub/tesla-http-proxy",
        "help": {
            "arguments": ["--help"],
            "exit_code": 0,
            "stdout": "",
            "stderr": "Usage: tesla-http-proxy [OPTION...]\n",
        },
    },
    "fleet_telemetry": {
        **packaged_sidecars["fleet_telemetry"],
        "path": "usr/lib/teslatlas-hub/fleet-telemetry",
        "help": {
            "arguments": ["--help"],
            "exit_code": 0,
            "stdout": "",
            "stderr": "maxprocs: <runtime>\nUsage of fleet-telemetry:\n",
        },
    },
}
module.validate_debian_attestation_sidecars(
    "amd64", help_records, packaged_sidecars
)
module.validate_debian_attestation_sidecars(
    "amd64", {"go_proxy": None, "fleet_telemetry": None}, None
)
tampered_help_records = {
    **help_records,
    "go_proxy": {
        **help_records["go_proxy"],
        "sha256": "4" * 64,
    },
}
try:
    module.validate_debian_attestation_sidecars(
        "amd64", tampered_help_records, packaged_sidecars
    )
except module.GateError:
    pass
else:
    raise AssertionError("tampered Debian sidecar attestation was accepted")
unexpected_execution = {
    **help_records,
    "fleet_telemetry": {
        **help_records["fleet_telemetry"],
        "help": {
            **help_records["fleet_telemetry"]["help"],
            "exit_code": 1,
        },
    },
}
try:
    module.validate_debian_attestation_sidecars(
        "amd64", unexpected_execution, packaged_sidecars
    )
except module.GateError:
    pass
else:
    raise AssertionError("failed Debian sidecar help execution was accepted")

replacement = root / "witness-replacement"
replacement.write_bytes(b"replacement evidence\n")
source.rename(root / "witness-source-original")
replacement.rename(source)
try:
    module.copy_witness_to(root, witness, root / "replaced-copy")
except module.GateError:
    pass
else:
    raise AssertionError("replaced evidence path was copied after capture")
assert not (root / "replaced-copy").exists()

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

external_manifest = "/Users/example/.cargo/registry/src/index/portable-1.0.0/Cargo.toml"
external_id = "registry+https://example.invalid/index#portable@1.0.0"
portable_fixture = {
    "packages": [{
        "id": external_id,
        "name": "portable",
        "version": "1.0.0",
        "source": "registry+https://example.invalid/index",
        "manifest_path": external_manifest,
        "targets": [{"src_path": external_manifest.replace("Cargo.toml", "src/lib.rs")}],
    }],
    "workspace_members": [external_id],
    "workspace_default_members": [external_id],
    "resolve": {"root": external_id, "nodes": [{"id": external_id, "deps": []}]},
}
portable = module.portable_cargo_metadata(portable_fixture, root)
portable_json = __import__("json").dumps(portable)
assert "/Users/example" not in portable_json
assert "cargo-registry://" in portable_json
assert portable["packages"][0]["id"].startswith("cargo:portable@1.0.0")

try:
    from compression import zstd
except ImportError:
    zstd = None
if zstd is not None:
    from io import BytesIO
    import tarfile

    raw_tar = BytesIO()
    with tarfile.open(fileobj=raw_tar, mode="w") as archive:
        data = b"Package: fixture\n"
        info = tarfile.TarInfo("control")
        info.size = len(data)
        archive.addfile(info, BytesIO(data))
    members = module.tar_regular_members(
        zstd.compress(raw_tar.getvalue()), "zstd fixture", "control.tar.zst"
    )
    assert members["control"] == b"Package: fixture\n"
PY
printf '%s\n' 'release-evidence fail-closed test passed'
