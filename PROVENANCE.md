# Provenance and independent-development record

## Purpose

This file records sources and treatment. It is **not** a blanket clean-room declaration.

## Original implementation

Teslatlas Hub is a Rust self-hosted collector, local store, sync service, CLI and platform controller maintained by MAGRATHEAN UK LTD.

## TeslaMate reference material

Compatibility work consulted public TeslaMate material, including source, schema, migrations, documentation, fixtures and observable behaviour.

Reviewed compatibility revision:

`7054517c10475f39f480edeae8f90c6f717985a3`

The repository contains TeslaMate-specific facts, names, schema mappings, fingerprints, behavioural compatibility and fixtures. It must not claim that no TeslaMate material or influence exists.

## Tesla Auth reference material

The native macOS OAuth onboarding flow adapts the endpoint constants, PKCE
authorization shape, `tesla://auth/callback` handling, China issuer routing,
and no-redirect 30-second token exchange from Tesla Auth `v0.15.0`, revision
`68da1f850e9cb87ac0e54c608d5a2e90d3ad1608` (MIT, © 2021 Adrian Kumpf).
The Wry/Tao GUI and Rust dependency graph are not bundled; macOS uses native
WebKit, CryptoKit, Security, and URLSession.

## Tesla Vehicle Command SDK

The macOS service and optional Debian Fleet package separately bundle Tesla's
official `tesla-http-proxy` from
`https://github.com/teslamotors/vehicle-command`, release `v0.4.1`, revision
`49977a18fd68567501d59e16a6c9e4a8b9348544`. It is Apache-2.0 licensed and is
built from the pinned upstream `go.mod`/`go.sum` with Go 1.27.0 exactly. The
macOS build uses `CGO_ENABLED=1`, `GOOS=darwin`, `GOARCH=arm64`, and
`MACOSX_DEPLOYMENT_TARGET=13.0`; Debian amd64 and ARM64 builds use
`CGO_ENABLED=0`. A private build copy adds only
`godebug default=go1.27`, preventing the older upstream module directive from
re-enabling legacy cryptographic compatibility defaults. No upstream source is
linked into the Rust Hub; the proxy remains a separately executed program.

Each macOS candidate includes deterministic Go source, dependency inventory,
SPDX SBOM, notices and a clean-rebuild receipt. The receipt records the proxy,
Go and compiler hashes plus GOROOT, Xcode and SDK identities. The signed app
binds both the unsigned proxy digest and evidence-manifest digest; release
checksums and signed candidate provenance bind every evidence component.

The package contains only the proxy executable and applicable notices. Its
command-authentication private key, TLS private key, certificate, OAuth
tokens, and session cache are created or supplied at runtime below the user's
private Hub data directory.

## Tesla Fleet Telemetry receiver bridge

The Fleet Telemetry bridge is built separately from Tesla's official receiver
at `https://github.com/teslamotors/fleet-telemetry`; the macOS service package
and optional Debian Fleet package bundle it. The reviewed release is `v0.9.4`, revision
`d64c73ab65e7c5fb5fc12b35fe507e2c6054227b` (Apache-2.0). The locked upstream
archive SHA-256 is
`a30818d9d832cf6dcec7cf0d61b780d4bea52cc7c9f8edb31a111bc0f25cd6b9`.

The build applies
`packaging/fleet-telemetry-bridge/0001-teslatlas-http-dispatcher.patch`
(SHA-256
`800b8572eb32da0f851316cf6e13349325ca6a3c4470887e8f3dc9bc222d8b59`)
to a temporary private source copy. That patch adds only the bounded Teslatlas
loopback dispatcher and its configuration. It forwards decoded `V` and
connectivity records to
`http://127.0.0.1:8080/v1/internal/fleet-telemetry`; vehicle records are
acknowledged only after Hub accepts them. The bridge is built with Go 1.27.0
exactly, `CGO_ENABLED=0`, `-trimpath`, and stripped symbols for Darwin amd64 and
ARM64 plus Linux amd64 and ARM64. The CGO-only Kafka and ZMQ integrations are
unavailable in this build; the Teslatlas runtime configuration selects only the
fixed loopback dispatcher.

The receiver remains a separately executed program; no upstream Go source is
linked into the Rust Hub. Public receiver certificate/key material and the
private loopback bearer are runtime deployment inputs and are never bundled.
Debian Fleet packages accept only the exact amd64 or ARM64 command-proxy and
receiver outputs recorded in `packaging/linux/sidecar-sha256.lock`. The package
embeds that candidate-bound lock and the selected pair of digests; caller-
supplied digest values cannot authorize different sidecar bytes.

## File classification

Every release file must be classified as:

- `MAGRATHEAN-ORIGINAL`
- `COMPANY-ASSIGNED-CONTRIBUTION`
- `TESLAMATE-COMPATIBILITY`
- `THIRD-PARTY`
- `GENERATED`
- `DATA-OR-FACTS`
- `UNKNOWN`

`UNKNOWN` blocks release.

## Protectable expression

A file containing copied, adapted or closely translated protectable TeslaMate expression must preserve applicable upstream rights and notices and remain under a compatible licence.

Facts, methods, protocols and interfaces are assessed separately from expression. Compatibility alone is not a legal conclusion either way.

## Automated scans

Exact-blob, shared-string and similarity scans are evidence tools only.

- An empty result does not prove independent creation.
- An unavailable repository is not a passed scan.
- A non-match does not resolve non-literal copying.
- A match does not prove infringement.

Record tool version, inputs, hashes, exclusions and adjudication.

## High-priority paths

Review all `teslamate*`, legacy authentication, Owner API, streaming and TeslaMate fixture paths before each release.

## Proprietary app boundary

No Hub implementation source may move into the proprietary app unless MAGRATHEAN UK LTD owns every relevant right or has a separate licence from every rightholder.

Shared protocol facts must be maintained separately from covered implementation.

## Baseline

Reviewed Hub `main`: `a2b8431028abb8d84465196fceb0c951de901cee`.
