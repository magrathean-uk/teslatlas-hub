# Release process

## Calendar releases

From 5 September 2026, use the [calendar versioning policy](versioning.md):
`YEAR.WEEK.REVISION`, initially agreed as `2026.36.1`. Keep historical tags
unchanged. Source, embedded product versions, packages, and release notes must
agree before publication. This policy does not itself publish or re-version
existing artifacts, or waive signing, provenance, and verification gates.

For 2026.36.1, prepare a new source commit and `v2026.36.1` tag. Preserve the
historical tags and their assets unchanged. GitHub stores source and authorised
release artifacts; builds and checks run on controlled hosts, with no GitHub
Actions workflow.

1. Verify version alignment, formatting, Rust checks/tests, AppKit tests, and
   packaging tests on the final source. Record exact completed results without
   substituting previous test logs.
2. Build the Apple-silicon macOS app/service package and Debian 13 ARM64/amd64
   packages. Inspect embedded versions, architecture, package metadata, runtime
   source routes, and legal notices. Retain exact dependency evidence for any
   reused unchanged companions. Omit companions when their admission gate fails
   and state that platform's functional limits prominently.
3. Record signing and notarisation results for the actual distributed bytes.
   An installed signing identity alone proves neither a signed installer nor
   notarisation. If publishing unsigned/unnotarised macOS artifacts, disclose
   that explicitly in the notes and build record.
4. Prepare only the reviewed artifacts:
   `TeslatlasHub-2026.36.1-arm64.pkg`,
   `teslatlas-hub_2026.36.1_arm64.deb`,
   `teslatlas-hub_2026.36.1_amd64.deb`, `SHA256SUMS`, and `BUILD-INFO.md`.
   Include or link exact corresponding source and required dependency source
   material. The build record must contain source commit, toolchain/platform,
   completed test results, package scope, and signing limitations.
5. Review all public records for credentials, device identifiers, private paths,
   logs, and backups. Do not upload the build directory wholesale.
6. After the final commit and matching artifact verification, create the new
   annotated tag and publish the authorised GitHub Release with its notes and
   selected files. Re-read remote tag resolution and downloaded asset hashes.
   Never move an existing published tag or replace its binaries.

No live Tesla or restored-backup acceptance is inferred from build, unit-test,
or package results. See [release notes](release-notes-2026.36.1.md),
[verification](verification.md), and [upgrade guidance](upgrade.md).

## Historical v1.0.0 workflow

The remaining instructions record the original v1.0.0 source-only release.
Do not rerun its tag-creation commands for a new release. Its restriction on
GitHub Release assets applies to that historical release, not to a separately
authorised future downloadable release.

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
