# Teslatlas Hub v1.0.0

Released 2026-08-31.

## Highlights

- Native macOS control app and combined `TeslatlasHub.pkg` installer.
- Native Debian 13 packaging for amd64 and ARM64.
- Legacy Owner API and Fleet API collection with multi-vehicle storage.
- Local authenticated sync, immutable history packs, backup, restore, and
  diagnostics.
- Read-only TeslaMate 4.2.0+ migration with visible progress and deterministic
  verification.

## Changes since v1.0.0-beta.1

- Reduced TeslaMate import setup delay and repeated per-row work.
- Added determinate import progress and a simpler migration flow.
- Preserved existing Hub credentials during repeat imports.
- Added a trusted-host check before SSH credentials or database credentials are
  used.
- Made service transitions bounded and suppressed stale UI completion events.
- Ensured stopping Hub closes collection, streaming, listeners, tunnels, and
  supervised companion processes.
- Combined the macOS app and service into one product installer.

## Distribution

The immutable source is the annotated `v1.0.0` tag. There is no GitHub Release
page and no downloadable GitHub release asset. The combined macOS package is
built locally as `dist/TeslatlasHub.pkg`.

Keep TeslaMate and Hub from concurrently owning the same legacy refresh-token
pair during migration cutover.
