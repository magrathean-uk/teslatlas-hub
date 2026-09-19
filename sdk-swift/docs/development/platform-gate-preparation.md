# F3 platform-gate preparation — 2026-09-19

This is preparation only. It does not accept the declared macOS 14, iOS 17 or
Linux ARM64 floors, and it does not replace the published source-handoff receipt.

The fail-closed inputs are in [`tools/platform-gates.json`](../../tools/platform-gates.json).
Run the current-host source and harness audit without starting a simulator or
container:

```sh
python3 tools/platform_gate.py verify
python3 tools/platform_gate.py command ios17
python3 tools/platform_gate.py command macos14
python3 tools/platform_gate.py command linux-arm64
```

The audit prepares and deletes a source-only handoff and requires all of these
previously accepted values:

- Protocol runtime-source commit
  `53b5c6483990db84e5214176755f398e93d87b1b`;
- source-package identity
  `733071fcd8c7db0e64b38547dab90dd354ad728dc2d9efee7fd2de438a14876e`;
- canonical staging root `teslatlas-sdk-swift`;
- 124 selected inputs totalling 769736 bytes; and
- all four public SwiftPM products, in their canonical order.

Because this candidate corrects current Docker documentation in `README.md`
and `docs/development.md`, the accepted package is materialized from immutable
published commit `d7ac4488fc5908015e8de55cd57983ea87172266`, not regenerated
from the dirty live checkout. That exact snapshot must reproduce the accepted
identity before any platform command can run.

## iOS 17

`iOSRuntimeHost/CurrentHubRuntimeHost.xcodeproj` now contains the shared
`CurrentHubRuntimeHost` scheme. Its app target and test bundle depend on all
four libraries. App launch executes `PlatformSurfaceProbe`, which imports and
uses a public type from each library before the XCTest bundle runs.

The prepared invocation uses the exact destination
`platform=iOS Simulator,OS=17.0,name=iPhone 15` and an isolated DerivedData
directory. `python3 tools/platform_gate.py run ios17` first requires an
available exact iOS 17.0/iPhone 15 simulator on an Apple-silicon host, then
runs that shared scheme. It has not been run in this preparation.

## macOS 14

`python3 tools/platform_gate.py run macos14` refuses every non-Apple-silicon
host and every macOS major other than 14. It prepares the accepted source-only
handoff from the locked source commit in a temporary directory and builds its
separately checksummed external consumer,
which imports all four libraries, with a separate SwiftPM scratch directory.
The temporary directory is deleted on exit. The current macOS 27 host is useful
for static checks but cannot accept the macOS 14 floor.

## native Linux ARM64

The Dockerfile uses the Docker Official Image `swift:6.0.3-jammy` and pins the
direct `linux/arm64/v8` child digest
`sha256:c84da0197afcc90ef90a64194d4d451be7c090a845bcbf632755f9c16334ba8f`.
Its parent OCI index is
`sha256:e2b0410500126d7f569d387b5817426cef5c38cc02dc494c3dc5edc8e10304d6`.
The official-images catalog records Swift 6.0.3 Jammy for both `amd64` and
`arm64v8` and source commit
`f44060cdf224436060d2df98a5c3f63f2600de63`; this lane deliberately selects
only the ARM64 child. Sources: [Swift Docker installation](https://www.swift.org/install/linux/docker/),
[Docker Official Swift image](https://hub.docker.com/_/swift/), and the
[Docker official-images Swift catalog](https://github.com/docker-library/official-images/blob/master/library/swift).

`python3 tools/platform_gate.py run linux-arm64` refuses a non-Linux or
non-ARM64 host, then independently requires the Docker Engine itself to report
Linux ARM64. It uses `--platform linux/arm64/v8`, never invokes
`matrix_wire.py`, and does not permit QEMU or an amd64 lane. The Docker build
context is the verified handoff itself: its canonical package and separately
checksummed `external-four-library-consumer`, with no dependency on the live
repository package tree. SwiftPM writes only to isolated `/tmp` scratch. The
runner removes its temporary context and owned image tag and does not run a
Hub. No Docker build or container was run for this preparation.

## Still open

Exact-floor executions and independent receipts remain required for macOS 14,
iOS 17 and native Linux ARM64. Live-Hub behavior, catalog install/update/
rollback/removal, F5 real-input semantics, and final F3, F6 and F7 acceptance
also remain open. App and Viewer are outside this harness.
