# Release verification

> **Historical reference:** this verifies a v1.0.0-beta.1 publication set. The
> current v1.0.0-beta.2 source candidate has no release assets. Signing,
> notarisation, and publication work are deferred.

Treat a download as an official Teslatlas Hub release only when the signed tag,
flat release checksums, detailed evidence, provenance signature, native Debian
receipts, platform signatures, exact source, and legal evidence all verify.

> **Historical beta.1 publication status:** v1.0.0-beta.1 is the first public beta. Treat it as
> official only when every named asset is present and the independent trust
> anchors match the company-controlled publication below.

## Independent trust anchors

Obtain these values from the authenticated MAGRATHEAN UK LTD publication at
<https://teslatlas.eu/hub/release-keys/v1.0.0-beta.1.txt>, outside the GitHub
release being checked:

- OpenPGP release fingerprint
  `A43B517A25C59994654639ED9CB5BEA1F3D65EDD`;
- SHA-256 of the provenance public key inside detailed evidence:
  `a787a55c4b93266453d86805a6cda1ba5b54c76ce31750a468c1dc76a7c18901`;
- SHA-256 of `TeslatlasHubDebianAttestationPublicKey.pem`:
  `7186087343ae93f3d9c5d02347f467a45937339118db1a5f043cb1f6d4e15fe7`.

Keys downloaded beside their signatures are evidence inputs, not independent
trust anchors.

## Verify the flat publication set

Place all downloaded release files in one directory. The script-generated set
is flat: unique platform artifact basenames, the tagged source tarball, one
detailed evidence tarball, `RELEASE_SIGNING_KEY.asc`, the Debian attestation
public key when Debian packages are present, `SHA256SUMS`, and detached
`SHA256SUMS.asc`. Both public-key files are covered by `SHA256SUMS`, but only
their independently authenticated full fingerprint or digest establishes
trust.

```sh
set -eu
RELEASE=/absolute/path/to/downloaded-v1.0.0-beta.1
EXPECTED_RELEASE_FINGERPRINT=A43B517A25C59994654639ED9CB5BEA1F3D65EDD
VERIFY_TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-release-verify.XXXXXX")
trap 'find "$VERIFY_TMP" -depth -delete 2>/dev/null || true' EXIT HUP INT TERM
SOURCE_CHECKOUT="$VERIFY_TMP/source"
VERIFY_GNUPGHOME="$VERIFY_TMP/gnupg"
install -d -m 0700 "$VERIFY_GNUPGHOME"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

sha256_check() {
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$1" && sha256sum -c "$2")
  else
    (cd "$1" && shasum -a 256 -c "$2")
  fi
}

EXPECTED_SIGNED_ASSETS="$VERIFY_TMP/expected-signed-assets"
EXPECTED_RELEASE_ASSETS="$VERIFY_TMP/expected-release-assets"
ACTUAL_RELEASE_ASSETS="$VERIFY_TMP/actual-release-assets"
MANIFEST_ASSETS="$VERIFY_TMP/manifest-assets"
MANIFEST_ASSETS_SORTED="$VERIFY_TMP/manifest-assets-sorted"
cat >"$EXPECTED_SIGNED_ASSETS" <<'EOF'
RELEASE_SIGNING_KEY.asc
Teslatlas Hub.zip
TeslatlasHubDebianAttestationPublicKey.pem
TeslatlasHubService.pkg
teslatlas-hub-v1.0.0-beta.1-evidence.tar.gz
teslatlas-hub-v1.0.0-beta.1-source.tar.gz
teslatlas-hub_1.0.0-beta.1_amd64.deb
teslatlas-hub_1.0.0-beta.1_arm64.deb
EOF
{
  cat "$EXPECTED_SIGNED_ASSETS"
  printf '%s\n' SHA256SUMS SHA256SUMS.asc
} | LC_ALL=C sort >"$EXPECTED_RELEASE_ASSETS"
find "$RELEASE" -mindepth 1 -maxdepth 1 -exec basename {} \; |
  LC_ALL=C sort >"$ACTUAL_RELEASE_ASSETS"
cmp "$EXPECTED_RELEASE_ASSETS" "$ACTUAL_RELEASE_ASSETS"
while IFS= read -r asset; do
  test -f "$RELEASE/$asset"
  test ! -L "$RELEASE/$asset"
done <"$EXPECTED_RELEASE_ASSETS"

awk '
  {
    digest = $1
    if (length(digest) != 64 || digest !~ /^[0-9a-f]+$/) exit 1
    plain_prefix = digest "  "
    binary_prefix = digest " *"
    if (index($0, plain_prefix) == 1) {
      name = substr($0, length(plain_prefix) + 1)
    } else if (index($0, binary_prefix) == 1) {
      name = substr($0, length(binary_prefix) + 1)
    } else {
      exit 1
    }
    if (name == "" || name ~ /^-/ || name ~ /\// || name ~ /\\/) exit 1
    print name
  }
' "$RELEASE/SHA256SUMS" >"$MANIFEST_ASSETS"
LC_ALL=C sort "$MANIFEST_ASSETS" >"$MANIFEST_ASSETS_SORTED"
cmp "$EXPECTED_SIGNED_ASSETS" "$MANIFEST_ASSETS_SORTED"

git clone https://github.com/magrathean-uk/teslatlas-hub.git "$SOURCE_CHECKOUT"
git -C "$SOURCE_CHECKOUT" fetch --force --tags origin
git -C "$SOURCE_CHECKOUT" checkout --detach v1.0.0-beta.1
test -z "$(git -C "$SOURCE_CHECKOUT" status --porcelain=v1 --untracked-files=all)"
test "$(git -C "$SOURCE_CHECKOUT" cat-file -t v1.0.0-beta.1)" = tag
cd "$SOURCE_CHECKOUT"

gpg --homedir "$VERIFY_GNUPGHOME" --batch \
  --import "$RELEASE/RELEASE_SIGNING_KEY.asc"
gpg --homedir "$VERIFY_GNUPGHOME" --batch \
  --fingerprint "$EXPECTED_RELEASE_FINGERPRINT"
TAG_STATUS=$(GNUPGHOME="$VERIFY_GNUPGHOME" git verify-tag --raw v1.0.0-beta.1 2>&1)
TAG_SIGNER=$(
  printf '%s\n' "$TAG_STATUS" |
    awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" {print $3}'
)
test "$TAG_SIGNER" = "$EXPECTED_RELEASE_FINGERPRINT"

CHECKSUM_STATUS=$(
  gpg --homedir "$VERIFY_GNUPGHOME" --batch --status-fd=1 \
    --verify "$RELEASE/SHA256SUMS.asc" "$RELEASE/SHA256SUMS" \
    2>/dev/null
)
CHECKSUM_SIGNER=$(
  printf '%s\n' "$CHECKSUM_STATUS" |
    awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" { print $3 }'
)
test "$CHECKSUM_SIGNER" = "$EXPECTED_RELEASE_FINGERPRINT"
sha256_check "$RELEASE" SHA256SUMS

```

Both tag and checksum `VALIDSIG` records must contain exactly the full expected
fingerprint, not a short key ID or merely any valid signature. Also confirm the
tag is annotated, its name is exactly `v` plus Cargo/Hub version, and its commit
is the source revision you intend to inspect.

Extract the already checksum-verified detailed archive into a fresh directory:

```sh
mkdir "$VERIFY_TMP/evidence"
tar -xzf "$RELEASE/teslatlas-hub-v1.0.0-beta.1-evidence.tar.gz" \
  -C "$VERIFY_TMP/evidence"
DETAIL="$VERIFY_TMP/evidence/teslatlas-hub-v1.0.0-beta.1-evidence"
test -d "$DETAIL"
```

## Verify detailed provenance

```sh
: "${PROVENANCE_PUBLIC_KEY_SHA256:?independent 64-hex pin required}"
openssl dgst -sha256 \
  -verify "$DETAIL/provenance-public-key.pem" \
  -signature "$DETAIL/provenance.sig" \
  "$DETAIL/provenance.json"
test "$(sha256_file "$DETAIL/provenance-public-key.pem")" = \
  "$PROVENANCE_PUBLIC_KEY_SHA256"
```

Confirm `provenance.json` and `artifact-manifest.json` name the expected tag,
commit, platform artifact digests, legal bundle, Rust source evidence, Fleet
upstream source, and both Debian receipt digests.

## Verify Debian packages and native attestations

```sh
dpkg-deb --field "$RELEASE/teslatlas-hub_1.0.0-beta.1_amd64.deb" \
  Package Version Architecture
dpkg-deb --field "$RELEASE/teslatlas-hub_1.0.0-beta.1_arm64.deb" \
  Package Version Architecture
dpkg-deb --contents "$RELEASE/teslatlas-hub_1.0.0-beta.1_amd64.deb"
dpkg-deb --contents "$RELEASE/teslatlas-hub_1.0.0-beta.1_arm64.deb"
```

The package name is `teslatlas-hub`; the package version is
`1.0.0~beta.1-1`; each architecture and ELF machine must agree. The exact
dependency legal bundle must be under
`/usr/share/doc/teslatlas-hub/dependency-legal`.

Verify both copies of the Ed25519 public key against the independently
published digest. The flat copy is independently checksummed by the signed
manifest and the detailed copy is provenance-bound, but neither copy is its
own trust anchor:

```sh
: "${DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256:?independent 64-hex pin required}"
DETAILED_ATTESTATION_KEY="$DETAIL/TeslatlasHubDebianAttestationPublicKey.pem"
SEPARATE_ATTESTATION_KEY="$RELEASE/TeslatlasHubDebianAttestationPublicKey.pem"
test "$(sha256_file "$DETAILED_ATTESTATION_KEY")" = \
  "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256"
test "$(sha256_file "$SEPARATE_ATTESTATION_KEY")" = \
  "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256"
cmp "$DETAILED_ATTESTATION_KEY" "$SEPARATE_ATTESTATION_KEY"
```

From a clean checkout containing the verified signed tag, verify both receipts:

```sh
python3 scripts/debian-release-attestation.py verify \
  --repo . \
  --tag v1.0.0-beta.1 \
  --tag-signer-fingerprint A43B517A25C59994654639ED9CB5BEA1F3D65EDD \
  --package "$RELEASE/teslatlas-hub_1.0.0-beta.1_amd64.deb" \
  --architecture amd64 \
  --receipt "$DETAIL/debian-native-attestations/amd64/debian-native-attestation.json" \
  --signature "$DETAIL/debian-native-attestations/amd64/debian-native-attestation.sig" \
  --public-key "$DETAILED_ATTESTATION_KEY" \
  --public-key-sha256 "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256"

python3 scripts/debian-release-attestation.py verify \
  --repo . \
  --tag v1.0.0-beta.1 \
  --tag-signer-fingerprint A43B517A25C59994654639ED9CB5BEA1F3D65EDD \
  --package "$RELEASE/teslatlas-hub_1.0.0-beta.1_arm64.deb" \
  --architecture arm64 \
  --receipt "$DETAIL/debian-native-attestations/arm64/debian-native-attestation.json" \
  --signature "$DETAIL/debian-native-attestations/arm64/debian-native-attestation.sig" \
  --public-key "$DETAILED_ATTESTATION_KEY" \
  --public-key-sha256 "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256"
```

Each receipt binds the signed tag, exact commit and version, architecture,
package and extracted-binary digests, native Debian 13 host, and toolchain. A
sidecar-bearing receipt also binds each sidecar's package path, size, digest,
and bounded native `--help` result: arguments, integer zero exit status, empty
stdout, and normalized usage stderr. This does not start either service or make
a telemetry connection.
The package verifier also admits the exact Linux sidecar digests only through
the tag-locked `packaging/linux/sidecar-sha256.lock`; the package embeds those
same per-architecture values as `SIDECAR_SHA256SUMS`. Emulation and unsigned
self-assertions are rejected.

The receipt proves which tag-locked sidecar bytes executed on the native host.
The per-architecture Go directory records the clean target rebuild performed on
the locked macOS generation host. The Fleet directory binds its receiver subject
and complete source/legal corpus without claiming a clean receiver rebuild.
Their verifiers validate the recorded evidence without rerunning a build, and
both directories must name the same binary subjects as the packages. Neither
substitutes for native Debian 13 package/runtime execution on amd64 and ARM64.

## Verify macOS topology and signatures

The ZIP contains the app. The app embeds a service-only package identical to
external `TeslatlasHubService.pkg`; the package never embeds the app.

```sh
ditto -x -k "$RELEASE/Teslatlas Hub.zip" "$VERIFY_TMP/macos"
APP="$VERIFY_TMP/macos/Teslatlas Hub.app"
EMBEDDED_PKG="$APP/Contents/Resources/TeslatlasHubService.pkg"
cmp "$RELEASE/TeslatlasHubService.pkg" "$EMBEDDED_PKG"
pkgutil --expand-full "$RELEASE/TeslatlasHubService.pkg" \
  "$VERIFY_TMP/service-expanded"
test ! -e "$VERIFY_TMP/service-expanded/Payload/Applications"

codesign --verify --deep --strict --verbose=2 "$APP"
spctl --assess --type execute --verbose=4 "$APP"
xcrun stapler validate "$APP"
pkgutil --check-signature "$RELEASE/TeslatlasHubService.pkg"
spctl --assess --type install --verbose=4 \
  "$RELEASE/TeslatlasHubService.pkg"
xcrun stapler validate "$RELEASE/TeslatlasHubService.pkg"

test "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasHubVersion' "$APP/Contents/Info.plist")" = \
  1.0.0-beta.1
test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")" = \
  1.0.0
test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist")" = \
  1.0.0b1
grep -Eq 'version="1\.0\.0b1"' "$VERIFY_TMP/service-expanded/PackageInfo"
```

The app and installer must have matching Developer ID Team IDs. The Hub remains
exactly `1.0.0-beta.1`; only Apple build/package metadata maps beta.1 to
`1.0.0b1`.

## Verify dependency source and legal evidence

```sh
RUST_CARGO=$(rustup which --toolchain 1.98.0 cargo)
PATH="$(dirname "$RUST_CARGO"):$PATH"
export PATH
python3 scripts/rust-source-evidence.py --repo . \
  --verify-dir "$DETAIL/rust-source-evidence" \
  --rebuild
python3 scripts/go-proxy-evidence.py --repo . \
  --verify-dir "$DETAIL/go-proxy-evidence"
python3 scripts/fleet-telemetry-evidence.py --repo . \
  --verify-dir "$DETAIL/fleet-telemetry-evidence"
for ARCH in amd64 arm64; do
  python3 scripts/go-proxy-evidence.py --repo . \
    --verify-dir "$DETAIL/linux-sidecar-evidence/$ARCH/go-proxy"
  python3 scripts/fleet-telemetry-evidence.py --repo . \
    --verify-dir "$DETAIL/linux-sidecar-evidence/$ARCH/fleet-telemetry"
done
python3 scripts/legal-bundle.py --repo . \
  --go-proxy-evidence "$DETAIL/go-proxy-evidence" \
  --fleet-telemetry-evidence "$DETAIL/fleet-telemetry-evidence" \
  --verify-dir "$DETAIL/dependency-legal"
test -f "$DETAIL/fleet-telemetry-evidence/fleet-telemetry-upstream-source.tar.gz"
test -f \
  "$DETAIL/fleet-telemetry-evidence/fleet-telemetry-go-module-sources.tar.gz"
test ! -e "$DETAIL/dependency-legal/go-component-manifest.json"
test ! -e \
  "$DETAIL/dependency-legal/fleet-telemetry-component-manifest.json"
```

Rust source evidence contains exactly `rust-vendored-sources.tar.gz`,
`rust-source-inventory.json`, and `rust-source-evidence-manifest.json`. Fleet
evidence must contain the exact pinned upstream archive
`fleet-telemetry-upstream-source.tar.gz`; a repository URL or digest alone is
not the corresponding source asset. It must also contain
`fleet-telemetry-go-module-sources.tar.gz`, with the exact source ZIP and
`go.mod` for all 45 locked runtime modules, including the Eclipse Paho
EPL-2.0 source. The Fleet notice embedded in each package points to this file
inside detailed evidence. This platform-invariant Fleet Go source/legal corpus
is evidence of source and legal completeness, not a native Linux reproduction
receipt.

The dependency legal bundle is platform-invariant. Architecture-bound
`go-component-manifest.json` and `fleet-telemetry-component-manifest.json`
remain in their complete evidence directories and are intentionally excluded
from that legal bundle. The staged `linux-sidecar-evidence/amd64` and
`linux-sidecar-evidence/arm64` trees must both verify and their subjects must
match the corresponding packaged binaries. The Go v2 evidence binds the target,
20-module cross-platform source lock, and 21 source packages before its clean
rebuild check.
