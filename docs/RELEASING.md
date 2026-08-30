# Release process

Only an authorised MAGRATHEAN UK LTD maintainer may publish an official release.
The repository's [release compliance gate](../RELEASE_COMPLIANCE.md) controls.

> **Current status:** v1.0.0-beta.1 is the first public beta. It is valid only as
> the complete atomic GitHub prerelease: signed tag, notarised macOS artifacts,
> native Debian 13 amd64 and ARM64 packages and receipts, exact source, detailed
> evidence, checksums, and detached signature. The independent release trust
> anchors are published at <https://teslatlas.eu/hub/release-keys/v1.0.0-beta.1.txt>.

## Fixed version mapping

The release version remains exactly `1.0.0-beta.1` in `Cargo.toml`, the Hub
binary, `TeslatlasHubVersion`, the tag `v1.0.0-beta.1`, and user-facing text.
Apple metadata deterministically maps that prerelease as follows:

- `CFBundleShortVersionString`: `1.0.0`;
- `CFBundleVersion`: `1.0.0b1`;
- service-package version: `1.0.0b1`.

The Debian package version is `1.0.0~beta.1-1`. Do not replace the Hub's exact
SemVer with either platform-specific value.

## Prepare and validate

1. Resolve every tracked-file provenance classification.
2. Confirm contributor and chain-of-title records privately.
3. Update `CHANGELOG.md`, the legal changelog, and release notes.
4. Run format, locked tests, warnings-denied Clippy, release build, AppKit
   tests, packaging tests, dependency audit, provenance audit, and all release
   evidence tests.
5. Build and runtime-test Debian amd64 and ARM64 on native Debian 13 hosts.
6. Provision the separate provenance and Debian-attestation keys under
   [RELEASE_KEYS.md](../RELEASE_KEYS.md), and publish the full release OpenPGP
   fingerprint through the separately authenticated company-controlled channel.
7. After every gate passes, perform the final candidate-status flip before
   tagging. Set the actual release date and replace every candidate, draft,
   preparation, and unreleased status for this version in `README.md`,
   `CHANGELOG.md`, `LEGAL_CHANGELOG.md`, `docs/README.md`,
   `docs/INSTALL_MACOS.md`, `docs/INSTALL_DEBIAN.md`, this guide,
   `RELEASE_KEYS.md`, `SOURCE_AVAILABILITY.md`, `RELEASE_VERIFICATION.md`,
   `RELEASE_COMPLIANCE.md`, `docs/FLEET_SETUP.md`, and the beta release notes.
   Remove version-specific blocker wording only after its factual gate passes.
   Change the release-note title to the final beta title, review the complete
   status/date diff, and commit it. The signed tag must point to that finalization
   commit; never make the flip after tagging.
8. Create a signed annotated tag at the reviewed commit and verify its exact
   signer fingerprint:

The current AppKit gate is `scripts/test-macos-appkit.sh`. It runs the unit
tests in the Debug configuration so Swift `@testable` imports are valid.
`scripts/build-macos-app.sh` separately performs and validates the production
Release build; do not enable testability in the shipped app merely to combine
those two gates.

The mandatory finalization check before step 8 is:

```sh
: "${RELEASE_DATE:?set the actual release date as YYYY-MM-DD}"
STATUS_RE=$(printf '%s' \
  'unpub''lished|release can''didate|draft no''tes|release prepa''ration|^## Unrel''eased|external publication block''er|blocks an official v1\.0\.0-beta\.1 release|blocks publication of v1\.0\.0-beta\.1|current missing external publication blocks v1\.0\.0-beta\.1|until the official beta is pub''lished')
STATUS_FILES=(
  README.md CHANGELOG.md LEGAL_CHANGELOG.md PRIVACY.md SECURITY.md
  docs/README.md docs/INSTALL_MACOS.md docs/INSTALL_DEBIAN.md
  docs/RELEASING.md docs/FLEET_SETUP.md
  docs/RELEASE_NOTES_v1.0.0-beta.1.md RELEASE_KEYS.md
  SOURCE_AVAILABILITY.md RELEASE_VERIFICATION.md RELEASE_COMPLIANCE.md
)
if rg -n "$STATUS_RE" "${STATUS_FILES[@]}"; then
  echo 'release status finalization is incomplete' >&2
  exit 1
fi
if rg -n --fixed-strings \
  'Supported only after the complete official beta release is published' \
  SECURITY.md; then
  echo 'security support status finalization is incomplete' >&2
  exit 1
fi
for dated in CHANGELOG.md LEGAL_CHANGELOG.md \
  docs/RELEASE_NOTES_v1.0.0-beta.1.md; do
  rg -q --fixed-strings "$RELEASE_DATE" "$dated" || {
    echo "release date missing from $dated" >&2
    exit 1
  }
done
```

```sh
EXPECTED_RELEASE_FINGERPRINT=A43B517A25C59994654639ED9CB5BEA1F3D65EDD
git tag -u A43B517A25C59994654639ED9CB5BEA1F3D65EDD -s \
  v1.0.0-beta.1 -m 'Teslatlas Hub v1.0.0-beta.1'
TAG_STATUS=$(git verify-tag --raw v1.0.0-beta.1 2>&1)
TAG_SIGNER=$(
  printf '%s\n' "$TAG_STATUS" |
    awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" {print $3}'
)
test "$TAG_SIGNER" = "$EXPECTED_RELEASE_FINGERPRINT"
test "$(git rev-parse HEAD)" = "$(git rev-parse 'v1.0.0-beta.1^{commit}')"
test -z "$(git status --porcelain=v1 --untracked-files=all)"
```

## Build the macOS inputs

On the approved Apple-silicon macOS release host with the pinned Rust, Go,
Xcode, and SDK versions selected:

```sh
scripts/build-macos-app.sh
python3 scripts/go-proxy-evidence.py --repo . \
  --verify-dir dist/go-proxy-evidence
python3 scripts/fleet-telemetry-evidence.py --repo . \
  --verify-dir dist/fleet-telemetry-evidence
python3 scripts/legal-bundle.py --repo . \
  --go-proxy-evidence dist/go-proxy-evidence \
  --fleet-telemetry-evidence dist/fleet-telemetry-evidence \
  --verify-dir dist/dependency-legal
```

The build produces exactly these release inputs:

- `dist/Teslatlas Hub.app`;
- `dist/TeslatlasHubService.pkg`;
- `dist/go-proxy-evidence/`;
- `dist/fleet-telemetry-evidence/`;
- `dist/dependency-legal/`.

The app contains `Contents/Resources/TeslatlasHubService.pkg`, byte-identical
to the external package. Both copies are service-only: the package has no
`Payload/Applications` and never contains the app. Fleet evidence includes the
exact pinned upstream source bytes as
`fleet-telemetry-upstream-source.tar.gz` and the exact source ZIP plus `go.mod`
for all 45 locked runtime modules as
`fleet-telemetry-go-module-sources.tar.gz`, including the Eclipse Paho
EPL-2.0 source.

Prepare both Linux sidecar subjects and their architecture-bound evidence on
this same approved macOS host. Go-proxy evidence generation is intentionally
locked to the reviewed Apple-silicon Go/Xcode build host even when its target is
Linux. Generation performs the clean target rebuild; later `--verify-dir`
checks validate the recorded subject and evidence without rebuilding it. Fleet
evidence binds the receiver subject and complete source/legal corpus; it does
not claim a clean receiver rebuild.

Run this block first with the amd64 values shown, then again with
`ARCH=arm64` and `SIDECAR_TARGET=linux-arm64`. Every output path must be absent
before its generation command:

```sh
ARCH=amd64
SIDECAR_TARGET=linux-amd64
PROXY_BINARY="dist/tesla-http-proxy-${ARCH}"
FLEET_BINARY="dist/fleet-telemetry-${ARCH}"
GO_EVIDENCE="dist/go-proxy-evidence-${ARCH}"
FLEET_EVIDENCE="dist/fleet-telemetry-evidence-${ARCH}"

scripts/build-tesla-command-proxy.sh \
  --target "$SIDECAR_TARGET" \
  --output "$PROXY_BINARY"
scripts/build-fleet-telemetry-bridge.sh \
  --target "$SIDECAR_TARGET" \
  --output "$FLEET_BINARY"
python3 scripts/go-proxy-evidence.py --repo . \
  --proxy-binary "$PROXY_BINARY" \
  --target "$SIDECAR_TARGET" \
  --output-dir "$GO_EVIDENCE"
python3 scripts/go-proxy-evidence.py --repo . \
  --verify-dir "$GO_EVIDENCE"
python3 scripts/fleet-telemetry-evidence.py --repo . \
  --receiver-binary "$FLEET_BINARY" \
  --target "$SIDECAR_TARGET" \
  --output-dir "$FLEET_EVIDENCE"
python3 scripts/fleet-telemetry-evidence.py --repo . \
  --verify-dir "$FLEET_EVIDENCE"
python3 scripts/legal-bundle.py --repo . \
  --go-proxy-evidence "$GO_EVIDENCE" \
  --fleet-telemetry-evidence "$FLEET_EVIDENCE" \
  --verify-dir dist/dependency-legal
```

Transfer each architecture's two binaries and two evidence directories, plus
the byte-identical platform-invariant `dist/dependency-legal`, to its native
Debian host. Do not regenerate Go evidence on Debian.

Sign, notarise, and staple release copies only:

```sh
: "${APPLE_APP_IDENTITY:?full Developer ID Application identity required}"
: "${APPLE_INSTALLER_IDENTITY:?full Developer ID Installer identity required}"
: "${APPLE_NOTARY_PROFILE:?Team-ID-bound notary profile required}"
mkdir -p dist/release
scripts/release-macos.sh \
  --app "dist/Teslatlas Hub.app" \
  --service-package dist/TeslatlasHubService.pkg \
  --go-proxy-evidence dist/go-proxy-evidence \
  --fleet-telemetry-evidence dist/fleet-telemetry-evidence \
  --legal-bundle dist/dependency-legal \
  --app-identity "$APPLE_APP_IDENTITY" \
  --installer-identity "$APPLE_INSTALLER_IDENTITY" \
  --notary-profile "$APPLE_NOTARY_PROFILE" \
  --output-dir dist/release/macos
```

`dist/release/macos` must not exist before the command. Its final contents are
`Teslatlas Hub.zip`, `TeslatlasHubService.pkg`, `go-proxy-evidence/`,
`fleet-telemetry-evidence/`, `dependency-legal/`, `notary-logs/`, and
`SHA256SUMS`. The ZIP contains the signed app; that app embeds the same signed,
stapled service-only package that is also downloadable separately.

## Generate Rust dependency source evidence

With all locked crates already present in the selected Cargo home:

```sh
RUST_CARGO=$(rustup which --toolchain 1.98.0 cargo)
PATH="$(dirname "$RUST_CARGO"):$PATH"
export PATH
python3 scripts/rust-source-evidence.py \
  --repo . \
  --cargo "$RUST_CARGO" \
  --cargo-home "${CARGO_HOME:-$HOME/.cargo}" \
  --bin teslatlas-hub \
  --output-dir dist/release/rust-source-evidence
python3 scripts/rust-source-evidence.py \
  --repo . \
  --verify-dir dist/release/rust-source-evidence \
  --rebuild
```

The output directory contains exactly:

- `rust-vendored-sources.tar.gz`;
- `rust-source-inventory.json`;
- `rust-source-evidence-manifest.json`.

The archive contains the exact Cargo.lock registry `.crate` archives and
unpacked sources, an offline Cargo source-replacement configuration, and no
workspace source. Verification independently reconstructs those sources,
reproduces the canonical archive, and performs a native offline locked rebuild.

## Build and attest Debian packages

Use one clean checkout of the signed tag on native Debian 13 amd64 and another
on native Debian 13 ARM64. The native proof is the exact Linux sidecar bytes
admitted by `packaging/linux/sidecar-sha256.lock` plus the signed native package
attestation; the package embeds the admitted per-architecture digests as
`SIDECAR_SHA256SUMS`. Use the architecture-bound evidence prepared from each
Linux receiver subject; do not copy Darwin component evidence and call it Linux
evidence.

The Fleet Go source/legal corpus is platform-invariant, but that fact alone is
not Linux reproducibility. Verify the transferred target-specific Go proxy and
Fleet receiver evidence for the package architecture. Each v2 Go manifest
binds its `target` and binary `subject` and records the clean rebuild completed
during macOS generation; verification does not rerun that rebuild. The
binary-specific Go and Fleet component manifests remain in their architecture
evidence directories and are excluded from the platform-invariant legal bundle.

The amd64 commands are below. On ARM64 set `ARCH=arm64`:

```sh
VERSION=1.0.0-beta.1
TAG=v1.0.0-beta.1
ARCH=amd64
PROXY_BINARY="dist/tesla-http-proxy-${ARCH}"
FLEET_BINARY="dist/fleet-telemetry-${ARCH}"
GO_EVIDENCE="dist/go-proxy-evidence-${ARCH}"
FLEET_EVIDENCE="dist/fleet-telemetry-evidence-${ARCH}"
: "${DEBIAN_ATTESTATION_SIGNING_KEY:?absolute key path outside the repository required}"

test -z "$(git status --porcelain=v1 --untracked-files=all)"
test "$(git rev-parse HEAD)" = "$(git rev-parse "$TAG^{commit}")"
mkdir -p dist
cargo build --locked --release --bin teslatlas-hub
python3 scripts/go-proxy-evidence.py --repo . \
  --verify-dir "$GO_EVIDENCE"
python3 scripts/fleet-telemetry-evidence.py --repo . \
  --verify-dir "$FLEET_EVIDENCE"
python3 scripts/legal-bundle.py \
  --repo . \
  --go-proxy-evidence "$GO_EVIDENCE" \
  --fleet-telemetry-evidence "$FLEET_EVIDENCE" \
  --verify-dir dist/dependency-legal
scripts/build-deb.sh \
  --binary target/release/teslatlas-hub \
  --command-proxy-binary "$PROXY_BINARY" \
  --fleet-telemetry-binary "$FLEET_BINARY" \
  --go-proxy-evidence "$GO_EVIDENCE" \
  --fleet-telemetry-evidence "$FLEET_EVIDENCE" \
  --legal-bundle dist/dependency-legal \
  --version "$VERSION" \
  --architecture "$ARCH" \
  --output "dist/teslatlas-hub_${VERSION}_${ARCH}.deb"
python3 scripts/debian-release-attestation.py generate \
  --repo . \
  --tag "$TAG" \
  --tag-signer-fingerprint A43B517A25C59994654639ED9CB5BEA1F3D65EDD \
  --package "dist/teslatlas-hub_${VERSION}_${ARCH}.deb" \
  --architecture "$ARCH" \
  --signing-key "$DEBIAN_ATTESTATION_SIGNING_KEY" \
  --output-dir "dist/debian-native-attestation-${ARCH}"
```

The signing key must be owner-only, outside the repository, and Ed25519 PEM.
`generate` requires a clean HEAD at the signed tag, validates the package
against tagged source, executes its extracted Hub binary with `--version`, and,
when sidecars are present, executes each extracted sidecar only with bounded
`--help`. The receipt binds each sidecar's package path, size, digest, arguments,
zero exit status, empty stdout, and normalized usage stderr. It records the
native host/toolchain, then writes only
`debian-native-attestation.json` and `debian-native-attestation.sig`.

Derive the public key once, name the release asset exactly as below, and compare
its digest with the independently approved trust anchor:

```sh
openssl pkey -in "$DEBIAN_ATTESTATION_SIGNING_KEY" -pubout \
  -out dist/release/TeslatlasHubDebianAttestationPublicKey.pem
DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256=$(
  shasum -a 256 dist/release/TeslatlasHubDebianAttestationPublicKey.pem |
    awk '{print $1}'
)
printf '%s\n' "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256"
```

Verify each transferred package and receipt on Linux or macOS:

```sh
python3 scripts/debian-release-attestation.py verify \
  --repo . \
  --tag v1.0.0-beta.1 \
  --tag-signer-fingerprint A43B517A25C59994654639ED9CB5BEA1F3D65EDD \
  --package dist/teslatlas-hub_1.0.0-beta.1_amd64.deb \
  --architecture amd64 \
  --receipt dist/debian-native-attestation-amd64/debian-native-attestation.json \
  --signature dist/debian-native-attestation-amd64/debian-native-attestation.sig \
  --public-key dist/release/TeslatlasHubDebianAttestationPublicKey.pem \
  --public-key-sha256 "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256"
```

Repeat verification with the ARM64 package, architecture, and receipt path.

## Generate candidate evidence

The evidence command requires a clean checkout at the signed tag, final
artifacts inside the repository, a nonexistent output directory, and the
custodian-supplied variables below. Both public-key digests must already be
recorded and published independently; do not derive and approve them inside the
same release operation.

```sh
: "${PROVENANCE_SIGNING_KEY:?production provenance key required}"
: "${PROVENANCE_PUBLIC_KEY_SHA256:?independent provenance pin required}"
: "${DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256:?independent Debian pin required}"
mkdir -p dist/release
python3 scripts/release-evidence.py \
  --repo . \
  --tag v1.0.0-beta.1 \
  --tag-signer-fingerprint A43B517A25C59994654639ED9CB5BEA1F3D65EDD \
  --output-dir dist/release/v1.0.0-beta.1 \
  --signing-key "$PROVENANCE_SIGNING_KEY" \
  --public-key-sha256 "$PROVENANCE_PUBLIC_KEY_SHA256" \
  --artifact "dist/release/macos/Teslatlas Hub.zip" \
  --artifact dist/release/macos/TeslatlasHubService.pkg \
  --artifact dist/teslatlas-hub_1.0.0-beta.1_amd64.deb \
  --artifact dist/teslatlas-hub_1.0.0-beta.1_arm64.deb \
  --legal-bundle dist/release/macos/dependency-legal \
  --rust-source-evidence dist/release/rust-source-evidence \
  --go-proxy-evidence dist/release/macos/go-proxy-evidence \
  --fleet-telemetry-evidence dist/release/macos/fleet-telemetry-evidence \
  --linux-go-proxy-evidence amd64=dist/go-proxy-evidence-amd64 \
  --linux-go-proxy-evidence arm64=dist/go-proxy-evidence-arm64 \
  --linux-fleet-telemetry-evidence amd64=dist/fleet-telemetry-evidence-amd64 \
  --linux-fleet-telemetry-evidence arm64=dist/fleet-telemetry-evidence-arm64 \
  --debian-attestation dist/debian-native-attestation-amd64 \
  --debian-attestation dist/debian-native-attestation-arm64 \
  --debian-attestation-public-key \
    dist/release/TeslatlasHubDebianAttestationPublicKey.pem \
  --debian-attestation-public-key-sha256 \
    "$DEBIAN_ATTESTATION_PUBLIC_KEY_SHA256"
```

The tool independently verifies both unique architecture receipts against the
captured packages, signed tag, exact version, commit, architecture, package
digest, and independently pinned key. It produces a flat publication directory
containing only:

- `Teslatlas Hub.zip`;
- `TeslatlasHubService.pkg`;
- `teslatlas-hub_1.0.0-beta.1_amd64.deb`;
- `teslatlas-hub_1.0.0-beta.1_arm64.deb`;
- `RELEASE_SIGNING_KEY.asc`;
- `TeslatlasHubDebianAttestationPublicKey.pem`;
- `teslatlas-hub-v1.0.0-beta.1-source.tar.gz`;
- `teslatlas-hub-v1.0.0-beta.1-evidence.tar.gz`;
- `SHA256SUMS`;
- `SHA256SUMS.asc`.

Every asset except the two checksum files is covered by `SHA256SUMS`.

The detailed archive contains provenance, SBOMs, inventories, notices, legal
bundle, Rust source evidence, macOS Go/Fleet evidence, per-architecture Linux
Go command-proxy rebuild evidence and Fleet subject/source/legal evidence, including
`fleet-telemetry-upstream-source.tar.gz` and
`fleet-telemetry-go-module-sources.tar.gz`, both native receipts/signatures, and
`TeslatlasHubDebianAttestationPublicKey.pem`. The embedded public key is for
verification convenience; its independently approved SHA-256 remains the
trust anchor. It is byte-identical to the independently checksummed flat
public-key asset. The Fleet notice embedded in the app and Debian packages
names the module-source archive; those exact source bytes live in this detailed
evidence archive, not in the legal-bundle directory embedded in each package.
That Fleet Go source/legal corpus is platform-invariant and must not be
described as a native Linux reproduction receipt.

## Publish

Create the GitHub draft prerelease from the exact tag. Its body is the finalized
beta notes plus immutable exact-tag links to the migration and legal changelog:

```sh
TAG=v1.0.0-beta.1
RELEASE_BODY=$(mktemp "${TMPDIR:-/tmp}/teslatlas-release-body.XXXXXX")
DRAFT_VERIFY=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-draft-verify.XXXXXX")
EXPECTED_ASSETS=$(mktemp "${TMPDIR:-/tmp}/teslatlas-assets.XXXXXX")
ACTUAL_ASSETS=$(mktemp "${TMPDIR:-/tmp}/teslatlas-actual-assets.XXXXXX")
trap 'rm -f "$RELEASE_BODY" "$EXPECTED_ASSETS" "$ACTUAL_ASSETS"; find "$DRAFT_VERIFY" -depth -delete 2>/dev/null || true' EXIT HUP INT TERM
cp docs/RELEASE_NOTES_v1.0.0-beta.1.md "$RELEASE_BODY"
printf '%s\n' \
  '' \
  '## Exact-tag documentation' \
  '' \
  '- [Migration](https://github.com/magrathean-uk/teslatlas-hub/blob/v1.0.0-beta.1/MIGRATION.md)' \
  '- [Legal changelog](https://github.com/magrathean-uk/teslatlas-hub/blob/v1.0.0-beta.1/LEGAL_CHANGELOG.md)' \
  >>"$RELEASE_BODY"
test "$(git rev-parse main)" = "$(git rev-parse "$TAG^{commit}")"
git push --atomic origin main "$TAG"
REMOTE_MAIN=$(git ls-remote --refs origin refs/heads/main | awk '{print $1}')
REMOTE_TAG_COMMIT=$(
  git ls-remote origin "refs/tags/$TAG^{}" | awk '{print $1}'
)
test "$REMOTE_MAIN" = "$(git rev-parse "$TAG^{commit}")"
test "$REMOTE_TAG_COMMIT" = "$REMOTE_MAIN"
gh release create "$TAG" \
  --repo magrathean-uk/teslatlas-hub \
  --title 'Teslatlas Hub v1.0.0-beta.1' \
  --notes-file "$RELEASE_BODY" \
  --verify-tag \
  --draft \
  --prerelease
gh release upload "$TAG" \
  dist/release/v1.0.0-beta.1/* \
  --repo magrathean-uk/teslatlas-hub

cat >"$EXPECTED_ASSETS" <<'EOF'
RELEASE_SIGNING_KEY.asc
SHA256SUMS
SHA256SUMS.asc
Teslatlas Hub.zip
TeslatlasHubDebianAttestationPublicKey.pem
TeslatlasHubService.pkg
teslatlas-hub-v1.0.0-beta.1-evidence.tar.gz
teslatlas-hub-v1.0.0-beta.1-source.tar.gz
teslatlas-hub_1.0.0-beta.1_amd64.deb
teslatlas-hub_1.0.0-beta.1_arm64.deb
EOF

test "$(gh release view "$TAG" --repo magrathean-uk/teslatlas-hub \
  --json isDraft --jq .isDraft)" = true
test "$(gh release view "$TAG" --repo magrathean-uk/teslatlas-hub \
  --json isPrerelease --jq .isPrerelease)" = true
gh release view "$TAG" --repo magrathean-uk/teslatlas-hub \
  --json body --jq .body |
  rg -q --fixed-strings \
    'github.com/magrathean-uk/teslatlas-hub/blob/v1.0.0-beta.1/MIGRATION.md'
gh release view "$TAG" --repo magrathean-uk/teslatlas-hub \
  --json body --jq .body |
  rg -q --fixed-strings \
    'github.com/magrathean-uk/teslatlas-hub/blob/v1.0.0-beta.1/LEGAL_CHANGELOG.md'

gh release download "$TAG" \
  --repo magrathean-uk/teslatlas-hub \
  --dir "$DRAFT_VERIFY"
find "$DRAFT_VERIFY" -mindepth 1 -maxdepth 1 -type f -exec basename {} \; |
  LC_ALL=C sort >"$ACTUAL_ASSETS"
cmp "$EXPECTED_ASSETS" "$ACTUAL_ASSETS"
while IFS= read -r asset; do
  test "$(shasum -a 256 "dist/release/v1.0.0-beta.1/$asset" | awk '{print $1}')" = \
    "$(shasum -a 256 "$DRAFT_VERIFY/$asset" | awk '{print $1}')"
done <"$EXPECTED_ASSETS"
(cd "$DRAFT_VERIFY" && shasum -a 256 -c SHA256SUMS)

gh release edit "$TAG" \
  --repo magrathean-uk/teslatlas-hub \
  --draft=false \
  --prerelease
test "$(gh release view "$TAG" --repo magrathean-uk/teslatlas-hub \
  --json isDraft --jq .isDraft)" = false
test "$(gh release view "$TAG" --repo magrathean-uk/teslatlas-hub \
  --json isPrerelease --jq .isPrerelease)" = true
```

Upload every file from the flat publication directory and no intermediate
evidence directory or its loose files. The commands verify the draft state,
prerelease state, exact ten asset basenames, byte-for-byte asset digests, and
exact-tag body links before changing the draft to published. After publication,
download the assets into a new empty directory and follow
[RELEASE_VERIFICATION.md](../RELEASE_VERIFICATION.md).

Do not publish when any signing identity, independent trust anchor, native
attestation, notarisation receipt, artifact, checksum, source archive, SBOM,
notice, legal bundle, or verification gate is missing.
