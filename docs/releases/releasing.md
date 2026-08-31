# Release process

Teslatlas Hub v1.0.0 is a source-tag release. GitHub stores the repository and
annotated `v1.0.0` tag; this release does not create a GitHub Release or upload
release assets.

## Prepare the source

1. Set the same SemVer in `Cargo.toml`, `Cargo.lock`, the macOS fallback
   metadata, tests, package examples, release notes, and public documentation.
2. Update `docs/releases/changelog.md`, `docs/legal/changelog.md`, and
   `docs/releases/release-notes-v1.0.0.md`.
3. Run the repository-layout and provenance checks.
4. Run formatting, warnings-denied Clippy, Rust tests, AppKit tests, and
   packaging tests.
5. Build and inspect the local platform packages.
6. Confirm the final tree contains no generated artifacts or secrets.

The release tag must match the Cargo package version exactly:

```sh
VERSION=$(sed -nE 's/^version = "([^"]+)"$/\1/p' Cargo.toml | head -n 1)
test "$VERSION" = 1.0.0
test -z "$(git status --porcelain=v1 --untracked-files=all)"
```

## Build the local macOS installer

On Apple silicon with Rust 1.98, Go 1.27.0, Xcode 27, and XcodeGen:

```sh
./scripts/build-macos-app.sh
test -d "dist/Teslatlas Hub.app"
test -f dist/TeslatlasHub.pkg
codesign --verify --deep --strict "dist/Teslatlas Hub.app"
pkgutil --payload-files dist/TeslatlasHub.pkg >/dev/null
```

`TeslatlasHub.pkg` installs the app in `/Applications` and the service payload
under `/Library/Application Support/Teslatlas Hub`. The app retains an embedded
service-only package for in-app service management.

The build also produces the dependency legal bundle and component evidence in
`dist/`. These files remain local unless the maintainer separately chooses to
distribute a package.

## Build a local Debian package

Build on the target Debian 13 architecture. The Hub binary, sidecars, evidence,
legal bundle, architecture, and requested version must agree:

```sh
cargo build --locked --release --bin teslatlas-hub
scripts/build-deb.sh \
  --binary target/release/teslatlas-hub \
  --command-proxy-binary dist/tesla-http-proxy-amd64 \
  --fleet-telemetry-binary dist/fleet-telemetry-amd64 \
  --go-proxy-evidence dist/go-proxy-evidence-amd64 \
  --fleet-telemetry-evidence dist/fleet-telemetry-evidence-amd64 \
  --legal-bundle dist/dependency-legal \
  --version 1.0.0 \
  --architecture amd64 \
  --output dist/teslatlas-hub_1.0.0_amd64.deb
```

Use the corresponding ARM64 inputs and `--architecture arm64` on ARM64.

## Tag and publish the source

Create the tag only after the final commit and verification are complete. The
repository protects `v*` tags from deletion and non-fast-forward updates.

```sh
git push origin main
git -c tag.gpgSign=false tag --annotate --no-sign \
  v1.0.0 -m 'Teslatlas Hub v1.0.0'
test "$(git rev-parse HEAD)" = "$(git rev-parse 'v1.0.0^{commit}')"
git push origin v1.0.0
```

Do not run `gh release create` and do not upload `dist/`. GitHub automatically
exposes the tagged source tree at
<https://github.com/magrathean-uk/teslatlas-hub/tree/v1.0.0>.
