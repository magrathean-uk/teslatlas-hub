# Verification status

This page records the current bounded evidence for product `2026.36.2`. It
distinguishes accepted source/runtime journeys from installation, publication,
real-data, and full-platform claims that have not been accepted.

## Canonical bindings

| Contract | Revision | Manifest SHA-256 |
| --- | --- | --- |
| Current-Hub HTTP | `hub-http-v1@1.0.0` | `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926` |
| Edge delivery | `edge-delivery-v2@2.0.0` | `e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224` |

G3 r2 admitted and read back the five active compatibility records. The receipt
is
[`g3-compatibility-admission-2026-09-19-r2.json`](development/g3-compatibility-admission-2026-09-19-r2.json)
(SHA-256
`5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`).
The focused six-case checker proved active-product-only operation, idempotence,
fail-closed receipt/profile identity checks, divergence rejection, exact
readback, and byte identity of the TypeScript vendored Protocol record.

## Accepted bounded evidence

| Gate | Target and consumer | Accepted behavior | Receipts | Limit |
| --- | --- | --- | --- | --- |
| G4 | macOS 27.0 Apple silicon; packed `@teslatlas/sdk@2026.36.2`; Node 26.7.0 and Chrome 153 | Normal TLS, browser rejection before scoped trust and after trust removal, seven CORS preflights, discovery/readiness, claim/replay rejection, two current results, drives `2/2/1`, cursor plus `304`, and credential rotation | [Hub G2/G4](../../hub/docs/development/active-macos-arm64-hub-typescript-g2-g4-2026-09-18-r1.json); [TypeScript G4](../../teslatlas-sdk-typescript/docs/development/macos-arm64-packed-node-browser-g4-2026-09-18-r1.json) | Source-built synthetic Hub and reviewed packed SDK only; no installer, service, real data, or minimum-floor result |
| G5 | Debian GNU/Linux 13.6 ARM64; source-built Edge and Hub | One guarded synthetic event survived Hub outage and Edge restart, retained stable retry identity, committed before occurrence-bound ACK, projected exactly one record, drained pending state, and survived Hub restart | [Hub G5](../../hub/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json); [Edge G5](../../teslatlas-edge/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json) | Synthetic source-built lane only; no package, installer, service manager, Tesla account, or vehicle |
| G6 | macOS 27.0 Apple silicon; Swift 6.4 external SwiftPM consumer | Exact discovery/profile/product/capabilities, normal TLS, claim/replay rejection, health/readiness, two current results, drives `2/2/1` with three `304`s, rotation, old-token rejection, and restart continuity | [Hub G6](../../hub/docs/development/g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json); [Swift G6](../../teslatlas-sdk-swift/docs/development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json) | Synthetic source-built external consumer only; declared macOS 14 SDK and macOS 13 Hub floors were not exercised |

The accepted cohorts were closed with their owned private roots, processes,
listeners, credentials, certificates, and build lock absent as recorded in the
receipts.

## What remains unaccepted

- No App integration or Viewer acceptance is claimed by these gates.
- No installer, notarization, package-service, service-manager, or release
  artifact acceptance is claimed.
- No Tesla account, real vehicle, named-source collection, real-data parity,
  production, or public-ingress acceptance is claimed.
- Declared minimum OS floors and a full platform matrix were not exercised.
- x86, x86_64, amd64, and Intel targets remain outside the active scope.
- The compatibility admission does not state that source was published or
  tagged.

Run local schema and conformance checks when changing contract bytes, but do
not treat those checks as substitutes for the accepted runtime receipts above.
