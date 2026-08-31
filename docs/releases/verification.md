# Verify v1.0.0

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
