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
mkdir "$TMP/signed-repo/scripts"
cp "$ROOT/scripts/go-proxy-evidence.py" "$TMP/signed-repo/scripts/"
cp "$ROOT/scripts/tesla-proxy-lock.json" "$TMP/signed-repo/scripts/"
cp "$ROOT/scripts/fleet-telemetry-evidence.py" "$TMP/signed-repo/scripts/"
mkdir -p "$TMP/signed-repo/packaging/fleet-telemetry-bridge"
cp "$ROOT/packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json" \
    "$TMP/signed-repo/packaging/fleet-telemetry-bridge/"
git -C "$TMP/signed-repo" add Cargo.toml main.rs LICENSE THIRD_PARTY_NOTICES.md Cargo.lock scripts packaging
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

rm "$TMP/signed-repo/artifact.deb"
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
mkdir -p "$TMP/substituted-repo/packaging/fleet-telemetry-bridge"
cp "$ROOT/packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json" \
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
python3 "$ROOT/scripts/fleet-telemetry-evidence.py" --repo "$TMP/substituted-repo" \
    --receiver-binary "$TMP/fleet-telemetry" \
    --output-dir "$TMP/substituted-fleet-telemetry-evidence" >/dev/null
if python3 "$ROOT/scripts/fleet-telemetry-evidence.py" --repo "$ROOT" \
    --verify-dir "$TMP/substituted-fleet-telemetry-evidence" \
    >"$TMP/substituted-fleet-telemetry.out" 2>&1; then
    echo 'release-evidence test: substituted Fleet Telemetry lock was accepted' >&2
    exit 1
fi
grep -Fq 'does not match the reviewed repository lock' \
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
grep -Eq 'valid ZIP archive|exactly one app' "$TMP/forged-mac.out"
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
printf '%s\n' service-package \
    >"$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg"
python3 - "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Info.plist" \
    "$TMP/tesla-http-proxy" "$TMP/fleet-telemetry" "$TMP/go-evidence/go-component-manifest.json" \
    "$TMP/fleet-telemetry-evidence/fleet-telemetry-component-manifest.json" \
    "$TMP/mac-artifact/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg" <<'PY'
import hashlib
from pathlib import Path
import plistlib
import sys

info, proxy, receiver, evidence, receiver_evidence, package = map(Path, sys.argv[1:])
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
info.write_bytes(plistlib.dumps({
    "CFBundleExecutable": "Teslatlas Hub",
    "CFBundleIdentifier": "eu.teslatlas.hub.fixture",
    "CFBundlePackageType": "APPL",
    "TeslatlasOfficialRelease": True,
    "TeslatlasReleaseTeamIdentifier": "4AA2EMZ2HA",
    "TeslatlasUnsignedProxySHA256": sha(proxy),
    "TeslatlasGoEvidenceManifestSHA256": sha(evidence),
    "TeslatlasUnsignedFleetTelemetrySHA256": sha(receiver),
    "TeslatlasFleetTelemetryEvidenceManifestSHA256": sha(receiver_evidence),
    "TeslatlasServicePackageSHA256": sha(package),
}))
PY
(cd "$TMP/mac-artifact" && /usr/bin/ditto -c -k --keepParent \
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
        mkdir -p "$3/Payload/Library/Application Support/Teslatlas Hub/bin"
        cp "$RELEASE_TEST_PROXY" \
            "$3/Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
        cp "$RELEASE_TEST_FLEET_TELEMETRY" \
            "$3/Payload/Library/Application Support/Teslatlas Hub/bin/fleet-telemetry"
        if [ "${RELEASE_MUTATE_PACKAGE_PROXY:-0}" = 1 ]; then
            printf '%s\n' changed >> \
                "$3/Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
        fi
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
(cd "$TMP/mac-artifact" && /usr/bin/ditto -c -k --keepParent \
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
[ -s "$TMP/signed-repo/mac-evidence/go-proxy-evidence/go-component-manifest.json" ]
(cd "$TMP/signed-repo" && shasum -a 256 -c mac-evidence/SHA256SUMS >/dev/null)
python3 - "$TMP/signed-repo/mac-evidence" <<'PY'
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
for receipt in app-log.json app-submit.json service-package-log.json \
    service-package-submit.json; do
    printf '%s\n' '{"status":"Accepted"}' >"$bundle/notary-logs/$receipt"
done
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
    } >SHA256SUMS
)
GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/bundle-evidence" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$bundle/Teslatlas Hub.zip" \
    --artifact "$bundle/TeslatlasHubService.pkg" \
    --go-proxy-evidence "$bundle/go-proxy-evidence" \
    --fleet-telemetry-evidence "$bundle/fleet-telemetry-evidence" >/dev/null
[ -s "$TMP/signed-repo/bundle-evidence/macos-release-receipts/SHA256SUMS" ]
[ -s "$TMP/signed-repo/bundle-evidence/macos-release-receipts/notary-logs/app-log.json" ]
(cd "$TMP/signed-repo" && shasum -a 256 -c bundle-evidence/SHA256SUMS >/dev/null)
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

printf '%s\n' 'fn main() { println!("later"); }' >"$TMP/signed-repo/main.rs"
git -C "$TMP/signed-repo" add main.rs
git -C "$TMP/signed-repo" commit -q -m later
printf '%s\n' later-artifact >"$TMP/signed-repo/later-artifact.deb"
if GNUPGHOME="$GPG_TMP" python3 "$SCRIPT" --repo "$TMP/signed-repo" --tag v1.0.0 \
    --tag-signer-fingerprint "$TAG_FINGERPRINT" \
    --output-dir "$TMP/signed-repo/wrong-head" --signing-key "$TMP/signing.pem" \
    --public-key-sha256 "$PUBLIC_KEY_SHA256" \
    --artifact "$TMP/signed-repo/later-artifact.deb" >"$TMP/wrong-head.out" 2>&1; then
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

source = root / "witness-source"
source.write_bytes(b"descriptor-pinned evidence\n")
witness = module.capture_artifact(root, source)
copied = root / "witness-copy"
module.copy_witness_to(root, witness, copied)
assert copied.read_bytes() == source.read_bytes()

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
PY
printf '%s\n' 'release-evidence fail-closed test passed'
