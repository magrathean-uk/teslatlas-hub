# Release compliance gates

## Calendar release gate

For 2026.36.1 and later, use the [calendar release process](releasing.md),
[versioning policy](versioning.md), and [verification guide](verification.md).
The exact source, embedded versions, package scope, legal/source material,
checksums, signing status, and completed test record must agree. Public records
must exclude private build paths, device identifiers, credentials, and backups.
No GitHub Actions build or verification is part of this process.

## Historical v1.0.0 release gate

The v1.0.0 release is complete when its source commit and annotated tag satisfy
this gate. GitHub Release assets are outside the v1.0.0 publication scope.

## Source and version

- `Cargo.toml`, `Cargo.lock`, CLI output, macOS metadata, package metadata,
  release notes, and public documentation identify version `1.0.0`.
- The annotated `v1.0.0` tag points to the reviewed release commit.
- The tagged tree is clean and contains no generated packages, build caches,
  credentials, private telemetry, or local release material.
- `LICENSE` remains the canonical GNU AGPL v3 text and project metadata uses
  `AGPL-3.0-only`.
- Tracked modifications to third-party components retain their required source,
  revision, licence, and change notices.

## Implementation

- Rust formatting, locked tests, warnings-denied Clippy, AppKit tests, packaging
  tests, and repository-layout verification pass on the release tree.
- Backup, restore, migration rollback, service lifecycle, and data-integrity
  behavior have focused regression coverage.
- No debug endpoint, fixture credential, or development-only bypass is enabled
  in the release build.
- Stopping Hub closes collection, streaming, listeners, SSH tunnels, and
  supervised companion processes.

## Local packages

- `scripts/build-macos-app.sh` produces `dist/TeslatlasHub.pkg` containing the
  app component and service component.
- The app, embedded Hub CLI, service component, and installer metadata agree on
  version `1.0.0`.
- Debian packages are built natively for their target architecture and contain
  matching Hub, sidecar, legal-bundle, and evidence inputs.
- Local package installation, first run, migration, repeat import, diagnostics,
  start, restart, stop, and removal flows are tested before the tag is pushed.

## Documentation and legal

- `.github/README.md`, `.github/SECURITY.md`, `docs/index.md`, installation
  guides, release notes, changelogs, Corresponding Source guidance, and this
  gate describe the same source-only publication model.
- The docs clearly state that v1.0.0 has no GitHub Release page or downloadable
  GitHub release assets.
- Local package builders retain the applicable licence, notices, dependency
  inventories, and source-evidence material.
- An operator who distributes or hosts a modified build is told to offer the
  source of the version actually distributed or run.

## Final check

```sh
test "$(git branch --show-current)" = main
test -z "$(git status --porcelain=v1 --untracked-files=all)"
test "$(git cat-file -t v1.0.0)" = tag
test "$(git rev-parse HEAD)" = "$(git rev-parse 'v1.0.0^{commit}')"
git diff --check HEAD^ HEAD
```
