# Verify a release

## Historical 2026.36.1 package verification

GitHub release downloads are no longer provided. The instructions below apply
only to previously retained packages and build records. For new installations,
[build from source](../guides/build-from-source.md) and retain your own source
commit, toolchain information and checksums.

Download the package for your architecture, `SHA256SUMS`, and `BUILD-INFO.md`
from the same `v2026.36.1` release. Check the build record's source commit,
platform scope, signing status, and completed checks. On macOS run
`shasum -a 256 -c SHA256SUMS`; on Debian run `sha256sum -c SHA256SUMS`.
If only one package was downloaded, other entries will report missing files;
the selected package must report `OK`. A checksum confirms byte identity with
the manifest, not a signing identity or live vehicle acceptance.

Inspect the selected package:

```sh
# macOS
pkgutil --check-signature TeslatlasHub-2026.36.1-arm64.pkg
pkgutil --payload-files TeslatlasHub-2026.36.1-arm64.pkg

# Debian (run on the target architecture)
dpkg-deb --field "teslatlas-hub_2026.36.1_$(dpkg --print-architecture).deb" \
  Package Version Architecture
dpkg-deb --contents "teslatlas-hub_2026.36.1_$(dpkg --print-architecture).deb"
```

The Debian metadata must report package `teslatlas-hub`, version
`2026.36.1-1`, and the selected architecture. These Debian packages omit Fleet
companions. The macOS package includes the app and service components. Its
installer is unsigned and unnotarised; the app uses ad-hoc signing. Neither is
Developer ID distribution. In-app embedded service installation and update
are unavailable; use the combined package for installation and upgrades.

After installation, use the platform's absolute binary path from the
[CLI reference](../guides/cli.md#platform-invocation) to run `--version`,
`legal`, and `source`. The version must be `2026.36.1` and source must identify
the matching immutable `v2026.36.1` tree. Preserve the legal bundle.

To inspect the corresponding source:

```sh
git clone https://github.com/magrathean-uk/teslatlas-hub.git teslatlas-hub-2026.36.1
cd teslatlas-hub-2026.36.1
git fetch --tags origin
git show --no-patch --format=fuller v2026.36.1
git rev-parse 'v2026.36.1^{commit}'
git checkout --detach v2026.36.1
```

Compare the resolved commit to `BUILD-INFO.md`. Tags and checksums are not a
claim of independent cryptographic authentication unless their signatures are
explicitly supplied and verified.

## Historical v1.0.0 verification

Teslatlas Hub v1.0.0 is a source-tag release. It has no GitHub Release page or
downloadable GitHub release assets.

## Verify the source tag

```sh
set -eu
git clone https://github.com/magrathean-uk/teslatlas-hub.git teslatlas-hub-v1
cd teslatlas-hub-v1
git fetch --tags origin
test "$(git cat-file -t v1.0.0)" = tag
git show --no-patch --format=fuller v1.0.0
git checkout --detach v1.0.0
test -z "$(git status --porcelain=v1 --untracked-files=all)"
test "$(sed -nE 's/^version = "([^"]+)"$/\1/p' Cargo.toml | head -n 1)" = 1.0.0
```

The annotated tag should name Teslatlas Hub v1.0.0 and its tagger should match
the project identity documented in `CITATION.cff`.

## Verify a local macOS build

Build from the detached tag, then inspect the combined installer:

```sh
./scripts/build-macos-app.sh
test -d "dist/Teslatlas Hub.app"
test -f dist/TeslatlasHub.pkg
test "$("dist/Teslatlas Hub.app/Contents/Resources/teslatlas-hub" --version)" = \
  'teslatlas-hub 1.0.0'
test "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasHubVersion' \
  'dist/Teslatlas Hub.app/Contents/Info.plist')" = 1.0.0

VERIFY_DIR=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-pkg.XXXXXX")
trap 'find "$VERIFY_DIR" -depth -delete 2>/dev/null || true' EXIT HUP INT TERM
pkgutil --expand-full dist/TeslatlasHub.pkg "$VERIFY_DIR/product"
test -d "$VERIFY_DIR/product/TeslatlasHubApp.pkg/Payload/Teslatlas Hub.app"
test -f "$VERIFY_DIR/product/TeslatlasHubService.pkg/Payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
test ! -e "$VERIFY_DIR/product/TeslatlasHubService.pkg/Payload/Applications"
```

The product must contain one app component installed in `/Applications` and one
service component installed at the filesystem root. The app embeds the same
service-only package for later in-app service management.

## Verify a local Debian build

```sh
dpkg-deb --field dist/teslatlas-hub_1.0.0_amd64.deb \
  Package Version Architecture
dpkg-deb --contents dist/teslatlas-hub_1.0.0_amd64.deb
```

The package must report `teslatlas-hub`, version `1.0.0-1`, and the architecture
of its Hub and sidecar binaries. Repeat with the ARM64 package on ARM64.

## Verify source and notices

```sh
./target/release/teslatlas-hub legal
./target/release/teslatlas-hub licence
./target/release/teslatlas-hub source
python3 scripts/verify-repository-layout.py
```

The source route must identify the immutable v1.0.0 tree. The package legal
bundle must contain the applicable project and third-party licence material.
