#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/release-evidence.py"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-release-evidence-test.XXXXXX")
GPG_TMP=
cleanup() {
    if [ -n "$GPG_TMP" ] && [ -d "$GPG_TMP" ]; then
        GNUPGHOME="$GPG_TMP" gpgconf --kill gpg-agent >/dev/null 2>&1 || true
        find "$GPG_TMP" -depth -delete 2>/dev/null || true
    fi
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$TMP/repo"
git -C "$TMP/repo" init -q
git -C "$TMP/repo" config user.name Test
git -C "$TMP/repo" config user.email test@example.invalid
printf '%s\n' '[package]' 'name = "fixture"' 'version = "1.0.0"' 'edition = "2024"' \
    >"$TMP/repo/Cargo.toml"
printf '%s\n' 'fn main() {}' >"$TMP/repo/main.rs"
printf '%s\n' artifact >"$TMP/repo/artifact.bin"
git -C "$TMP/repo" add Cargo.toml main.rs
git -C "$TMP/repo" commit -q -m fixture
git -C "$TMP/repo" tag -a -m fixture v1.0.0

if python3 "$SCRIPT" --repo "$TMP/repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$(printf '%064d' 0)" \
    --output-dir "$TMP/repo/evidence" --signing-key "$TMP/missing.pem" \
    --public-key-sha256 "$(printf '%064d' 0)" \
    --artifact "$TMP/repo/artifact.bin" >"$TMP/out" 2>&1; then
    echo 'release-evidence test: missing signing key was accepted' >&2
    exit 1
fi
grep -Fq 'regular, non-symlink file' "$TMP/out"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
    -out "$TMP/signing.pem" >/dev/null 2>&1
if python3 "$SCRIPT" --repo "$TMP/repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$(printf '%064d' 0)" \
    --output-dir "$TMP/retry" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$(printf '%064d' 0)" \
    --artifact "$TMP/repo/artifact.bin" >"$TMP/retry.out" 2>&1; then
    echo 'release-evidence test: unsigned tag was accepted' >&2
    exit 1
fi
grep -Fq 'cryptographically verified' "$TMP/retry.out"

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
    >"$TMP/signed-repo/Cargo.toml"
printf '%s\n' 'fn main() {}' >"$TMP/signed-repo/main.rs"
printf '%s\n' 'MIT License' >"$TMP/signed-repo/LICENSE"
printf '%s\n' '# notices' >"$TMP/signed-repo/THIRD_PARTY_NOTICES.md"
printf '%s\n' '# lockfile' 'version = 4' '' '[[package]]' \
    'name = "fixture"' 'version = "1.0.0"' >"$TMP/signed-repo/Cargo.lock"
git -C "$TMP/signed-repo" add Cargo.toml main.rs LICENSE THIRD_PARTY_NOTICES.md Cargo.lock
git -C "$TMP/signed-repo" commit -q -m fixture
GNUPGHOME="$GPG_TMP" git -C "$TMP/signed-repo" tag -s -m fixture v1.0.0
printf '%s\n' artifact >"$TMP/signed-repo/artifact.deb"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
    -out "$TMP/signing.pem" >/dev/null 2>&1
openssl pkey -in "$TMP/signing.pem" -pubout -out "$TMP/signing.pub" >/dev/null 2>&1
PUBLIC_KEY_SHA256=$(shasum -a 256 "$TMP/signing.pub" | awk '{ print $1 }')

REAL_CARGO=$(command -v cargo)
mkdir "$TMP/mutate-bin"
cat >"$TMP/mutate-bin/cargo" <<'EOF'
#!/bin/sh
printf '%s\n' changed >>"$RELEASE_MUTATION_ARTIFACT"
exec "$RELEASE_REAL_CARGO" "$@"
EOF
chmod 700 "$TMP/mutate-bin/cargo"
if PATH="$TMP/mutate-bin:$PATH" RELEASE_REAL_CARGO="$REAL_CARGO" \
    RELEASE_MUTATION_ARTIFACT="$TMP/signed-repo/artifact.deb" \
    GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.deb" >"$TMP/mutation.out" 2>&1; then
    echo 'release-evidence test: artifact mutation was accepted' >&2
    exit 1
fi
grep -Fq 'artifact changed during evidence generation' "$TMP/mutation.out"
[ ! -e "$TMP/signed-repo/evidence" ] \
    || { echo 'release-evidence test: failed generation exposed partial output' >&2; exit 1; }
printf '%s\n' artifact >"$TMP/signed-repo/artifact.deb"

GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.deb" >/dev/null
[ -s "$TMP/signed-repo/evidence/teslatlas-hub-v1.0.0-source.tar.gz" ]
[ -s "$TMP/signed-repo/evidence/sbom.spdx.json" ]
[ -s "$TMP/signed-repo/evidence/THIRD_PARTY_NOTICES.generated.md" ]
(cd "$TMP/signed-repo" && shasum -a 256 -c evidence/SHA256SUMS >/dev/null)
openssl dgst -sha256 -verify "$TMP/signed-repo/evidence/provenance-public-key.pem" \
    -signature "$TMP/signed-repo/evidence/provenance.sig" \
    "$TMP/signed-repo/evidence/provenance.json" >/dev/null
MANIFEST_ARTIFACT_SHA=$(python3 -c \
    'import json,sys; print(json.load(open(sys.argv[1]))["artifacts"][0]["sha256"])' \
    "$TMP/signed-repo/evidence/artifact-manifest.json")
CHECKSUM_ARTIFACT_SHA=$(awk '$2 == "artifact.deb" { print $1 }' \
    "$TMP/signed-repo/evidence/SHA256SUMS")
[ "$MANIFEST_ARTIFACT_SHA" = "$CHECKSUM_ARTIFACT_SHA" ] \
    || { echo 'release-evidence test: manifest/checksum artifact digests differ' >&2; exit 1; }
mv "$TMP/signed-repo/evidence" "$TMP/evidence-finished"

printf '%s\n' 'fn main() { println!("later"); }' >"$TMP/signed-repo/main.rs"
git -C "$TMP/signed-repo" add main.rs
git -C "$TMP/signed-repo" commit -q -m later
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-head" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/artifact.deb" >"$TMP/wrong-head.out" 2>&1; then
    echo 'release-evidence test: clean HEAD outside signed tag was accepted' >&2
    exit 1
fi
grep -Fq 'candidate HEAD does not match the signed tag commit' "$TMP/wrong-head.out"
[ ! -e "$TMP/signed-repo/wrong-head" ]

python3 - "$SCRIPT" "$TMP" <<'PY'
import importlib.util
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location("release_evidence_test", sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
root = Path(sys.argv[2])
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
PY
printf '%s\n' 'release-evidence fail-closed test passed'
